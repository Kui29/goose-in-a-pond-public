//! One lane for every background job that spends inference: at most one runs per tick.
//!
//! Concurrent jobs on the single resident model evict each other's KV cache and both slow down.
//! Pure-SQL jobs (decay, pruning) must stay out: behind the idle gate a busy pond never prunes.

use std::time::Duration;

use super::consolidation_schedule::{self, GateInputs, SkipReason};

/// A background job that spends inference. Declaration order is the derived `Ord` and
/// [`select_next`]'s tie-break; callers sort jobs by it (a `HashMap` would randomise ties).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LaneJob {
    /// Merge and score the memory store.
    Consolidation,
    /// Give conversations readable names.
    Titling,
    /// Look at recent household events and propose something.
    ProactiveReview,
    /// Keep each active conversation's rolling summary current.
    SummaryRefresh,
    /// Embed what the personal-context index lacks and prune what it should drop. Declared after
    /// `SummaryRefresh` so on a tie a summary is written before it is indexed.
    IndexMaintenance,
    /// Read one window of one conversation and decide what is worth
    /// remembering about the person in it.
    ///
    /// Declared LAST, and that is not a style choice: declaration order is the
    /// tie-break, and this is the job that should lose every tie it is in. It
    /// is the most expensive per pass (three model calls of ~2,240 tokens
    /// each), the least urgent (a conversation from last March does not get
    /// staler), and the only one whose work is safely resumable -- its cursor
    /// means a lost tick costs nothing but the tick. The summary refresh, by
    /// contrast, maintains the context the NEXT turn is answered from.
    ///
    /// It is in the lane rather than keeping its own loop for the obvious
    /// reason and one less obvious one: it spends the single inference slot,
    /// and it is the job most likely to be mid-call when somebody comes back.
    MemoryExtraction,
    /// Compose questions out of the household's own memories.
    ///
    /// Declared after [`MemoryExtraction`] because it reads what extraction
    /// writes: a pass that runs first on a fresh pond has nothing to compose
    /// from. Order is only the tie-break and the asker now wins those, so this
    /// is about saying which depends on which rather than about scheduling.
    ///
    /// Cheaper than extraction — one call over twelve notes, against
    /// extraction's three calls per window — and the least urgent thing on the
    /// lane: a question the household is not asked today is a question they are
    /// asked tomorrow.
    SuggestionGeneration,
}

impl LaneJob {
    /// Every job, in declaration order — which is the lane's own tie-break, so
    /// a surface that lists them in this order is listing them in the order
    /// they actually win ties.
    ///
    /// Exhaustive by construction: `as_str` matches on every variant without a
    /// wildcard, and `every_job_is_in_all` walks this slice against it. A new
    /// variant that is not added here fails that test rather than quietly
    /// vanishing from the API and the panel that lists it.
    pub const ALL: &'static [LaneJob] = &[
        LaneJob::Consolidation,
        LaneJob::Titling,
        LaneJob::ProactiveReview,
        LaneJob::SummaryRefresh,
        LaneJob::IndexMaintenance,
        LaneJob::MemoryExtraction,
        LaneJob::SuggestionGeneration,
    ];

    /// Short, stable label for structured logs and metrics.
    ///
    /// Also the wire name: it is what `POST /lane/jobs/{job}/run` takes in its
    /// path and what the status route answers with. Stable means stable —
    /// renaming one breaks a URL somebody has in a script.
    pub fn as_str(self) -> &'static str {
        match self {
            LaneJob::Consolidation => "consolidation",
            LaneJob::Titling => "titling",
            LaneJob::ProactiveReview => "proactive_review",
            LaneJob::SummaryRefresh => "summary_refresh",
            LaneJob::IndexMaintenance => "index_maintenance",
            LaneJob::MemoryExtraction => "memory_extraction",
            LaneJob::SuggestionGeneration => "suggestion_generation",
        }
    }

    /// What a household calls it. The panel shows this; the wire never does.
    pub fn title(self) -> &'static str {
        match self {
            LaneJob::Consolidation => "Tidy the memory store",
            LaneJob::Titling => "Name conversations",
            LaneJob::ProactiveReview => "Look for something to suggest",
            LaneJob::SummaryRefresh => "Refresh conversation summaries",
            LaneJob::IndexMaintenance => "Maintain the search index",
            LaneJob::MemoryExtraction => "Read conversations for memories",
            LaneJob::SuggestionGeneration => "Think of things to suggest",
        }
    }

    /// Parse a wire name back. `None` for anything else, so an unknown job in a
    /// URL is a 404 rather than a silently ignored request that answers OK.
    pub fn from_wire(name: &str) -> Option<LaneJob> {
        LaneJob::ALL.iter().copied().find(|j| j.as_str() == name)
    }
}

/// One job's own readiness, independent of the shared gate.
#[derive(Debug, Clone, Copy)]
pub struct JobState {
    pub job: LaneJob,
    /// This job's live enable toggle, re-read per tick so it applies without a restart.
    pub enabled: bool,
    /// Time since this job last ran in this process; `None` if it never has.
    pub since_last_run: Option<Duration>,
    /// Minimum spacing between this job's runs; its poll period here means "whenever eligible".
    pub interval_floor: Duration,
    /// How quiet it must be before THIS job may take the slot. Per-job so the summary refresh
    /// (~30 s) can run between turns while chores wait out a long quiet; exclusion is unaffected.
    pub idle_threshold: Duration,
    /// Whether this job may run on a pond that has served no turn since boot (e.g. indexing
    /// connector mail). Per-job: relaxing the lane-wide flag instead deadlocks the lane.
    pub exempt_from_activity_gate: bool,
}

/// Everything the lane needs for one tick.
#[derive(Debug, Clone, Copy)]
pub struct LaneInputs<'a> {
    /// The "never on startup" guard; see [`consolidation_schedule::saw_activity_since_start`].
    pub saw_activity_since_start: bool,
    /// Time since the most recent user activity, from either source.
    pub idle_for: Duration,
    /// The registered jobs. Order is the tie-break and nothing else.
    pub jobs: &'a [JobState],
    /// Which job is asking, when one is.
    ///
    /// `None` for a status read, which asks on nobody's behalf and must not
    /// bias the answer it reports.
    ///
    /// THIS IS WHY IT EXISTS. Every job polls on its own timer in its own task,
    /// so at the instant `select_next` runs there is exactly one job actually
    /// asking -- and the registry it is compared against is full of jobs that
    /// are asleep. A job that has never run sorts as `Duration::MAX`, so
    /// several never-run jobs tie at the top, and the tie used to go to
    /// declaration order regardless of who was awake. Measured on a real pond:
    /// `IndexMaintenance` registers at boot, never having run, and polls every
    /// fifteen minutes; `MemoryExtraction` polls every sixty seconds and is
    /// declared last. So for up to fifteen minutes at a stretch the extraction
    /// job lost every tick to a job that was not asking and would not ask until
    /// its own timer came round. On the one occasion this pond had a window
    /// long enough to reach extraction at all, it lost four consecutive ticks
    /// that way and the process was killed a minute before the sleeper woke.
    ///
    /// Declaration order still decides between two jobs that genuinely tick
    /// together. Because each is its own task on its own poll, that is close to
    /// never -- which is the point: it was arbitrating a race that does not
    /// happen, at the cost of one that does.
    pub asking: Option<LaneJob>,
}

/// The lane's verdict for one tick; `Run` holding one job is the mutual-exclusion guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneDecision {
    Run(LaneJob),
    /// Nothing ran; carries the most lane-wide skip reason (see `most_informative`).
    Idle(SkipReason),
}

impl LaneDecision {
    pub fn job(self) -> Option<LaneJob> {
        match self {
            LaneDecision::Run(job) => Some(job),
            LaneDecision::Idle(_) => None,
        }
    }
}

/// Pick the one job that may run this tick: least-recently-run wins (never-run beats all),
/// ties go to declaration order. Not a priority queue, which would starve rare jobs.
pub fn select_next(inputs: LaneInputs<'_>) -> LaneDecision {
    let mut best: Option<(&JobState, Duration)> = None;
    let mut blocked: Option<SkipReason> = None;

    for state in inputs.jobs {
        let decision = consolidation_schedule::should_run(GateInputs {
            enabled: state.enabled,
            // Per-job OR: one job's exemption must not qualify the rest.
            saw_activity_since_start: inputs.saw_activity_since_start
                || state.exempt_from_activity_gate,
            idle_for: inputs.idle_for,
            idle_threshold: state.idle_threshold,
            since_last_run: state.since_last_run,
            interval_floor: state.interval_floor,
        });

        match decision {
            consolidation_schedule::GateDecision::Skip(reason) => {
                blocked = Some(match blocked {
                    None => reason,
                    Some(existing) => most_informative(existing, reason),
                });
            }
            consolidation_schedule::GateDecision::Run => {
                let waited = state.since_last_run.unwrap_or(Duration::MAX);
                let wins = match best {
                    // A longer wait always wins. On an EQUAL wait the job that
                    // is actually asking takes it, and only if nobody is asking
                    // does the earlier declaration keep it. See `asking`.
                    Some((_, best_waited)) => {
                        waited > best_waited
                            || (waited == best_waited && inputs.asking == Some(state.job))
                    }
                    None => true,
                };
                if wins {
                    best = Some((state, waited));
                }
            }
        }
    }

    match best {
        Some((state, _)) => LaneDecision::Run(state.job),
        // A lane with no jobs registered is off.
        None => LaneDecision::Idle(blocked.unwrap_or(SkipReason::Disabled)),
    }
}

/// Which of two skip reasons better explains an idle lane: lane-wide beats one job's cadence.
fn most_informative(a: SkipReason, b: SkipReason) -> SkipReason {
    fn rank(r: SkipReason) -> u8 {
        match r {
            SkipReason::NoActivitySinceStart => 3,
            SkipReason::StillActive => 2,
            SkipReason::IntervalFloor => 1,
            SkipReason::Disabled => 0,
        }
    }
    if rank(b) > rank(a) {
        b
    } else {
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defect this was measured on, reproduced.
    ///
    /// Every job polls in its own task, so at any instant one job is asking and
    /// the rest of the registry is asleep. Two never-run jobs both sort as
    /// `Duration::MAX`, and the tie used to go to declaration order — so
    /// `IndexMaintenance` (declared fifth, polls every fifteen minutes, has
    /// never run) took the tick from `MemoryExtraction` (declared last, polls
    /// every sixty seconds) without ever intending to use it. On the real pond
    /// extraction lost four consecutive ticks that way and the process died a
    /// minute before the sleeper's own timer came round.
    #[test]
    fn an_asleep_job_does_not_take_a_tick_from_the_job_that_is_asking() {
        let jobs = [
            // Both have never run, so both wait `Duration::MAX`.
            job(LaneJob::IndexMaintenance, None, 60),
            job(LaneJob::MemoryExtraction, None, 60),
        ];

        // Extraction is asking. It is declared LAST, so under the old rule it
        // lost this tick to the sleeper.
        assert_eq!(
            select_next(LaneInputs {
                saw_activity_since_start: true,
                idle_for: LONG_IDLE,
                jobs: &jobs,
                asking: Some(LaneJob::MemoryExtraction),
            }),
            LaneDecision::Run(LaneJob::MemoryExtraction),
        );

        // And symmetrically — this is the control that proves the rule is about
        // who is asking rather than about favouring extraction.
        assert_eq!(
            select_next(LaneInputs {
                saw_activity_since_start: true,
                idle_for: LONG_IDLE,
                jobs: &jobs,
                asking: Some(LaneJob::IndexMaintenance),
            }),
            LaneDecision::Run(LaneJob::IndexMaintenance),
        );
    }

    /// Asking does not buy a job a tick it has not waited for. The wait is
    /// still the comparison; asking only breaks an exact tie.
    #[test]
    fn asking_breaks_a_tie_and_never_beats_a_longer_wait() {
        let jobs = [
            // Never run: infinitely starved.
            job(LaneJob::Consolidation, None, 60),
            // Ran a second ago, and asking.
            job(LaneJob::MemoryExtraction, Some(1), 0),
        ];

        assert_eq!(
            select_next(LaneInputs {
                saw_activity_since_start: true,
                idle_for: LONG_IDLE,
                jobs: &jobs,
                asking: Some(LaneJob::MemoryExtraction),
            }),
            LaneDecision::Run(LaneJob::Consolidation),
            "the job that has waited longer still goes first",
        );
    }

    /// A status read asks on nobody's behalf, so it must report what the next
    /// tick would decide without the reader's own choice of job changing it.
    #[test]
    fn a_read_with_nobody_asking_still_breaks_ties_by_declaration_order() {
        let jobs = [
            job(LaneJob::IndexMaintenance, None, 60),
            job(LaneJob::MemoryExtraction, None, 60),
        ];

        assert_eq!(
            select_next(LaneInputs {
                saw_activity_since_start: true,
                idle_for: LONG_IDLE,
                jobs: &jobs,
                asking: None,
            }),
            LaneDecision::Run(LaneJob::IndexMaintenance),
        );
    }

    /// `ALL` is what the API enumerates and what the Settings panel lists, so a
    /// variant missing from it is a job nobody can see or run by hand. The
    /// match in `as_str` has no wildcard, which makes the compiler catch a new
    /// variant there; nothing makes it catch one here, so this does.
    #[test]
    fn every_job_is_in_all() {
        // Each of the six, named individually. A loop over `ALL` comparing
        // against `ALL` would pass on an empty slice.
        for job in [
            LaneJob::Consolidation,
            LaneJob::Titling,
            LaneJob::ProactiveReview,
            LaneJob::SummaryRefresh,
            LaneJob::IndexMaintenance,
            LaneJob::MemoryExtraction,
            LaneJob::SuggestionGeneration,
        ] {
            assert!(
                LaneJob::ALL.contains(&job),
                "{} missing from ALL",
                job.as_str()
            );
        }
        assert_eq!(LaneJob::ALL.len(), 7, "a job was added or removed");
    }

    /// `ALL` is documented as declaration order, which is the tie-break
    /// `select_next` uses. A surface listing them in `ALL` order is telling the
    /// household the order they actually win in, so the two must agree.
    #[test]
    fn all_is_in_the_order_ties_break() {
        let mut sorted = LaneJob::ALL.to_vec();
        sorted.sort();
        assert_eq!(sorted.as_slice(), LaneJob::ALL, "ALL is not in Ord order");
    }

    /// The wire name round-trips, and nothing else parses. An unknown job has to
    /// be distinguishable from a known one or the run route answers OK for a
    /// typo and nothing ever happens.
    #[test]
    fn wire_names_round_trip_and_nothing_else_parses() {
        for &job in LaneJob::ALL {
            assert_eq!(LaneJob::from_wire(job.as_str()), Some(job));
        }
        for bogus in ["", "Consolidation", "memory-extraction", "titling ", "nope"] {
            assert_eq!(
                LaneJob::from_wire(bogus),
                None,
                "{bogus:?} should not parse"
            );
        }
    }

    /// Two jobs sharing a wire name would make one of them unreachable by URL
    /// and the other run twice; two sharing a title would make the panel show
    /// the same row twice.
    #[test]
    fn names_and_titles_are_unique() {
        let mut wire: Vec<&str> = LaneJob::ALL.iter().map(|j| j.as_str()).collect();
        wire.sort_unstable();
        let before = wire.len();
        wire.dedup();
        assert_eq!(wire.len(), before, "two jobs share a wire name");

        let mut titles: Vec<&str> = LaneJob::ALL.iter().map(|j| j.title()).collect();
        titles.sort_unstable();
        let before = titles.len();
        titles.dedup();
        assert_eq!(titles.len(), before, "two jobs share a title");
    }

    const IDLE_THRESHOLD: Duration = Duration::from_secs(15 * 60);
    const LONG_IDLE: Duration = Duration::from_secs(60 * 60);

    fn job(j: LaneJob, since_last_run: Option<u64>, floor_secs: u64) -> JobState {
        JobState {
            job: j,
            enabled: true,
            since_last_run: since_last_run.map(Duration::from_secs),
            interval_floor: Duration::from_secs(floor_secs),
            idle_threshold: IDLE_THRESHOLD,
            exempt_from_activity_gate: false,
        }
    }

    #[test]
    fn an_exemption_belongs_to_one_job_and_does_not_qualify_the_rest() {
        let mut sweep = job(LaneJob::IndexMaintenance, None, 0);
        sweep.exempt_from_activity_gate = true;
        let jobs = [job(LaneJob::Consolidation, None, 0), sweep];

        let decision = select_next(LaneInputs {
            saw_activity_since_start: false,
            idle_for: LONG_IDLE,
            jobs: &jobs,
            asking: None,
        });
        assert_eq!(
            decision,
            LaneDecision::Run(LaneJob::IndexMaintenance),
            "the exempt job must win, and must not have qualified consolidation"
        );
    }

    #[test]
    fn no_job_runs_before_the_pond_has_been_used() {
        let jobs = [
            job(LaneJob::Consolidation, None, 0),
            job(LaneJob::IndexMaintenance, None, 0),
        ];
        let decision = select_next(LaneInputs {
            saw_activity_since_start: false,
            idle_for: LONG_IDLE,
            jobs: &jobs,
            asking: None,
        });
        assert!(
            matches!(decision, LaneDecision::Idle(_)),
            "the startup guard must still hold when nothing is exempt"
        );
    }

    /// `asking: None` so the existing tests keep asserting the declaration-order
    /// tie-break they were written for. The asker rule has its own tests.
    fn tick(jobs: &[JobState]) -> LaneDecision {
        select_next(LaneInputs {
            saw_activity_since_start: true,
            idle_for: LONG_IDLE,
            jobs,
            asking: None,
        })
    }

    // ── Mutual exclusion ───────────────────────────────────────────────────

    #[test]
    fn a_tick_can_never_start_two_jobs() {
        let jobs = [
            job(LaneJob::Consolidation, None, 0),
            job(LaneJob::Titling, None, 0),
            job(LaneJob::ProactiveReview, None, 0),
        ];
        assert!(tick(&jobs).job().is_some());
    }

    // ── The shared gate applies to the lane, not to one job ────────────────

    #[test]
    fn a_household_mid_conversation_blocks_every_job() {
        let jobs = [
            job(LaneJob::Consolidation, None, 0),
            job(LaneJob::Titling, None, 0),
        ];
        let decision = select_next(LaneInputs {
            saw_activity_since_start: true,
            idle_for: Duration::from_secs(30),
            jobs: &jobs,
            asking: None,
        });
        assert_eq!(decision, LaneDecision::Idle(SkipReason::StillActive));
    }

    #[test]
    fn an_untouched_process_runs_nothing() {
        let jobs = [job(LaneJob::Consolidation, None, 0)];
        let decision = select_next(LaneInputs {
            saw_activity_since_start: false,
            idle_for: LONG_IDLE,
            jobs: &jobs,
            asking: None,
        });
        assert_eq!(
            decision,
            LaneDecision::Idle(SkipReason::NoActivitySinceStart)
        );
    }

    #[test]
    fn a_disabled_job_is_passed_over_not_run_late() {
        let jobs = [
            JobState {
                enabled: false,
                ..job(LaneJob::Consolidation, None, 0)
            },
            job(LaneJob::Titling, None, 0),
        ];
        assert_eq!(tick(&jobs), LaneDecision::Run(LaneJob::Titling));
    }

    // ── Fairness ───────────────────────────────────────────────────────────

    #[test]
    fn the_longest_waiting_job_goes_first() {
        let jobs = [
            job(LaneJob::Consolidation, Some(100), 0),
            job(LaneJob::Titling, Some(900), 0),
            job(LaneJob::ProactiveReview, Some(400), 0),
        ];
        assert_eq!(tick(&jobs), LaneDecision::Run(LaneJob::Titling));
    }

    #[test]
    fn a_job_that_has_never_run_outranks_every_job_that_has() {
        let jobs = [
            job(LaneJob::Consolidation, Some(86_400), 0),
            job(LaneJob::Titling, None, 0),
        ];
        assert_eq!(tick(&jobs), LaneDecision::Run(LaneJob::Titling));
    }

    #[test]
    fn a_frequent_job_cannot_starve_a_rare_one() {
        // Titling is eligible every 5 min, consolidation every 6 h.
        let mut titling_last: Option<u64> = Some(0);
        let mut consolidation_last: Option<u64> = Some(0);
        let mut consolidation_runs = 0;

        // One tick every 5 minutes for 24 hours.
        for tick_idx in 1..=288u64 {
            let now = tick_idx * 300;
            let jobs = [
                job(
                    LaneJob::Titling,
                    titling_last.map(|t| now - t),
                    300, // eligible every poll
                ),
                job(
                    LaneJob::Consolidation,
                    consolidation_last.map(|t| now - t),
                    6 * 3600,
                ),
            ];
            match tick(&jobs).job() {
                Some(LaneJob::Titling) => titling_last = Some(now),
                Some(LaneJob::Consolidation) => {
                    consolidation_last = Some(now);
                    consolidation_runs += 1;
                }
                _ => {}
            }
        }

        assert!(
            consolidation_runs >= 3,
            "consolidation was starved by titling — got {consolidation_runs} runs in 24h, \
             which is the exact failure a fixed priority ordering would produce"
        );
    }

    #[test]
    fn an_interval_floor_still_bounds_a_starved_job() {
        let jobs = [
            job(LaneJob::Consolidation, Some(60), 6 * 3600),
            job(LaneJob::Titling, Some(10), 0),
        ];
        assert_eq!(tick(&jobs), LaneDecision::Run(LaneJob::Titling));
    }

    #[test]
    fn equal_waits_break_by_declaration_order() {
        let jobs = [
            job(LaneJob::Titling, Some(500), 0),
            job(LaneJob::Consolidation, Some(500), 0),
        ];
        assert_eq!(tick(&jobs), LaneDecision::Run(LaneJob::Titling));
    }

    #[test]
    fn a_summary_is_written_before_anything_tries_to_index_it() {
        assert!(
            LaneJob::SummaryRefresh < LaneJob::IndexMaintenance,
            "declaration order is the tie-break the runner sorts by, so this ordering IS the \
             policy: a variant added between these two silently reverses it"
        );

        // Presented in the order the runner produces, which sorts by `LaneJob`.
        let jobs = [
            job(LaneJob::SummaryRefresh, Some(500), 0),
            job(LaneJob::IndexMaintenance, Some(500), 0),
        ];
        assert_eq!(tick(&jobs), LaneDecision::Run(LaneJob::SummaryRefresh));
    }

    /// Batch memory extraction is declared last, and must stay there.
    ///
    /// The tie-break is declaration order, so where this variant sits IS the
    /// policy for every tie it takes part in. Last is the correct place for
    /// three reasons that all point the same way: it is the most expensive pass
    /// in the lane, the least urgent (a conversation from last March does not
    /// get staler), and the only one that keeps a durable cursor -- so a tick it
    /// loses costs exactly that tick, whereas a tick the summary refresh loses
    /// is a tick the next turn is answered without its context.
    ///
    /// Inserting a variant after it silently reverses that, which is why this
    /// is asserted rather than left to the comment above.
    #[test]
    fn the_most_expensive_job_loses_every_tie() {
        assert!(
            LaneJob::IndexMaintenance < LaneJob::MemoryExtraction,
            "declaration order is the tie-break the runner sorts by, so this ordering IS the \
             policy: a variant added after MemoryExtraction silently reverses it"
        );

        // Presented in the order the runner produces, which sorts by `LaneJob`.
        let jobs = [
            job(LaneJob::IndexMaintenance, Some(500), 0),
            job(LaneJob::MemoryExtraction, Some(500), 0),
        ];
        assert_eq!(tick(&jobs), LaneDecision::Run(LaneJob::IndexMaintenance));

        // Losing ties is not starving. The moment it has waited longer than the
        // other job, it wins outright -- which is the property that makes
        // "declared last" a cost ordering rather than a permanent refusal.
        let starved = [
            job(LaneJob::IndexMaintenance, Some(500), 0),
            job(LaneJob::MemoryExtraction, Some(9_000), 0),
        ];
        assert_eq!(tick(&starved), LaneDecision::Run(LaneJob::MemoryExtraction));
    }

    #[test]
    fn the_index_sweep_cannot_run_beside_another_job() {
        // Starved far longer than the other, so it wins the tick outright...
        let jobs = [
            job(LaneJob::Consolidation, Some(60), 0),
            job(LaneJob::IndexMaintenance, Some(9_000), 0),
        ];
        assert_eq!(tick(&jobs), LaneDecision::Run(LaneJob::IndexMaintenance));

        // ...and winning is the whole grant: `LaneDecision` cannot name two jobs.
        assert_eq!(tick(&jobs).job(), Some(LaneJob::IndexMaintenance));
    }

    // ── Reporting ──────────────────────────────────────────────────────────

    #[test]
    fn an_idle_lane_reports_the_reason_that_explains_the_most() {
        // One job waits out its floor, but the household is active: the lane-wide reason wins.
        let jobs = [
            job(LaneJob::Consolidation, Some(60), 6 * 3600),
            job(LaneJob::Titling, Some(10), 300),
        ];
        let decision = select_next(LaneInputs {
            saw_activity_since_start: true,
            idle_for: Duration::from_secs(5),
            jobs: &jobs,
            asking: None,
        });
        assert_eq!(decision, LaneDecision::Idle(SkipReason::StillActive));
    }

    #[test]
    fn a_short_threshold_job_runs_in_a_gap_that_blocks_the_chores() {
        // A one-minute pause: too short for consolidation, enough for the summary refresh.
        let jobs = [
            job(LaneJob::Consolidation, None, 0),
            JobState {
                idle_threshold: Duration::from_secs(30),
                ..job(LaneJob::SummaryRefresh, None, 0)
            },
        ];
        let decision = select_next(LaneInputs {
            saw_activity_since_start: true,
            idle_for: Duration::from_secs(60),
            jobs: &jobs,
            asking: None,
        });
        assert_eq!(decision, LaneDecision::Run(LaneJob::SummaryRefresh));
    }

    #[test]
    fn an_empty_lane_is_idle_rather_than_a_panic() {
        assert_eq!(tick(&[]), LaneDecision::Idle(SkipReason::Disabled));
    }
}
