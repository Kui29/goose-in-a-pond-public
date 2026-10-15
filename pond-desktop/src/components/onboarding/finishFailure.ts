import { ApiError } from "../../api/types";

const FIELD_LABELS: Record<string, string> = {
  user_name: "your name",
  assistant_name: "the assistant's name",
  timezone: "your time zone",
};

/** What to tell the household when `POST /onboard/complete` refuses. */
export function finishFailure(e: unknown): string {
  const missing =
    e instanceof ApiError ? (e.body as { missing_fields?: unknown } | undefined)?.missing_fields : undefined;
  if (Array.isArray(missing) && missing.length > 0) {
    const fields = missing.map((f) => FIELD_LABELS[String(f)] ?? String(f)).join(", ");
    return `Setup can't finish yet: ${fields} is missing. Go back and fill it in.`;
  }
  return "Setup couldn't be finished. Check that the pond is running and try again.";
}
