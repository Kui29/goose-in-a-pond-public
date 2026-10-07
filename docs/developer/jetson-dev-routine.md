# Jetson dev routine

The Jetson Orin Nano (`nano.local`) is a standing deployment target: GIAP runs
there as an always-on service, and a one-command deploy pushes the current
`main` to it from the dev machine.

## Access

```bash
ssh nano          # alias in ~/.ssh/config → nano@nano.local, key ~/.ssh/nano_jetson
```

mDNS (`nano.local`) occasionally flakes; the box usually holds `192.168.1.14`
on the LAN, so an IP-based fallback alias is useful if resolution fails.

## The service

GIAP runs as a **user-level** systemd unit (no sudo needed to manage it, and
`loginctl enable-linger` is set, so it starts at boot and survives logout):

```bash
ssh nano systemctl --user status goose-in-a-pond    # status
ssh nano systemctl --user restart goose-in-a-pond   # restart
ssh nano 'cd goose-in-a-pond && bash scripts/giap.sh doctor'   # first move when something is wrong
ssh nano 'cd goose-in-a-pond && bash scripts/giap.sh logs'     # follow logs (knows the file path + UTC offset)
```

- Unit file: `~/.config/systemd/user/goose-in-a-pond.service` on the Jetson
- Binary: `~/goose-in-a-pond/target/release/pond-server` (CUDA build)
- Dashboard: loopback only, so tunnel to it: `ssh -N -L 8080:127.0.0.1:8080 nano`, then
  **http://localhost:8080**. Keep the same port on both ends or OAuth redirects miss. (8080
  because a user service cannot bind 80 without capabilities; the embedded web UI serves
  same-origin.) Phones reach the pond over pinned HTTPS, normally port 4443; see
  `docs/remote-access.md`.
- Logs: rolling files at `~/.local/share/goose-in-a-pond/logs/pond.log.YYYY-MM-DD`.
  **Not journald** — `journalctl --user -u goose-in-a-pond` is empty by design,
  which has repeatedly been misread as "the service is silent". Timestamps are
  UTC; the device clock is EAT (+3), so fresh logs look three hours stale.
- Inference: **in-process CUDA GGUF** (`gemma-4-E2B-it` Q4_K_M, n_ctx 16384,
  ~24-26 tok/s decode). Confirm with `giap.sh status`'s engine field, or:
  `grep "Applied Jetson Orin Nano CUDA" ~/.local/share/goose-in-a-pond/logs/pond.log.$(date +%F)`.
  `Applied Metal/platform settings` on this box means the binary was built
  WITHOUT the cuda feature and is running on the CPU. See
  `docs/developer/inference_optimization.md` for the fit-verdict rules and the
  NvMap drop_caches gotcha.

## Deploying

```bash
bash scripts/jetson.sh deploy                 # deploy origin/main
bash scripts/jetson.sh deploy --branch mybr   # any branch pushed to the Jetson's origin
```

The script: hard-resets the Jetson checkout to the pushed branch (submodule
included), builds the web UI **on the dev machine** (the Jetson's Node is v12;
Vite needs ≥ 18) and rsyncs `pond-desktop/dist` over, runs the on-device CUDA
release build (`--features pond-adapters-local-inference/cuda`;
`target-cpu=native` is correct on-device), restarts the user service, and
health-checks `GET /api/v1/health`.

Deploys build on the Jetson's `origin` remote (the GitHub mirror), so push
there first — or push to `jarida-io` and sync the mirror.

## LLM provider & context tuning

- **Provider: Ollama** (`llama3.2:3b`, ~22 tok/s warm) is the working path on the
  Jetson, verified end-to-end with the full giap-* tool surface. First request
  after idle pays a one-time cold model-load (~70s); warm responses are ~8s for a
  short prompt. Keep the model resident with `OLLAMA_KEEP_ALIVE`.
- **Leave `context_window_override` at 0.** It used to be set to 16384 here for
  the Ollama path, and that advice is now actively harmful: on the in-process
  GGUF engine the registry-pinned `context_size` (16384) wins anyway, and a
  larger override only makes GIAP budget history the engine cannot hold. See
  `docs/jetson-build-and-run.txt` under "Known open items".
- **Power mode: 25W (`sudo nvpmodel -m 1`), not MAXN_SUPER.** Measured on one
  identical turn: MAXN_SUPER produced 1,176 over-current events versus 44 at
  25W, for a 5% decode difference. Both modes run memory at the same 3199 MHz,
  and GIAP decode is memory-bandwidth-bound, so MAXN_SUPER buys almost nothing
  here.
- **Direct llama.cpp (`llama-server`) is NOT wired in.** It was evaluated
  (research move R6) and deferred: Goose streams from the OpenAI endpoint and the
  box's June llama.cpp build's `--jinja` streaming tool-call parser errors on
  GIAP's tool payload ("expected peg-native format"), and that build benchmarked
  *slower* than Ollama (18.6 vs 22 tok/s). Revisit with a fresh llama.cpp build.
- **GPU clocks**: the governor idles at 306 MHz but ramps to 1020 MHz under load
  on its own — pinning via `jetson_clocks` (needs sudo) is a latency/consistency
  win, not a throughput multiplier.

## Gotchas

- **No passwordless sudo** on the Jetson — anything touching system units,
  apt, or root-owned files needs an interactive `ssh -t nano "sudo …"`.
- A cold CUDA build takes well over an hour on the Nano; the deploy script's
  build step is incremental after the first run. Keep `target/release`;
  `target/debug` is pure waste on this box (`cargo clean` reclaims ~20GB).
- The GPU is memory-bandwidth-bound: keep models ≤ the GPU budget
  (`tok/s ≈ 102 / model_GB`, ~1GB headroom for KV cache).
- In-process Whisper keeps its decode states for the life of the process. With
  `base` (measured 2026-10-05; see `docs/voice-pipeline-efficiency.md`) the
  accurate state holds about 208 MB plus its CUDA scratch pool, and local voice
  mode adds a 170.6 MB wake-word state on first use. Count both in that budget
  next to the LLM; they are not freed between utterances.
