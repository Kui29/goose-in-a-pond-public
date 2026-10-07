import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";
import { beforeEach, describe, expect, it } from "vitest";

const html = readFileSync("../crates/pond-mcp-server/apps/weather-card.html", "utf8");
const script = new DOMParser().parseFromString(html, "text/html").querySelector("script")!.textContent!;
let render: (data: Record<string, unknown>) => void;

beforeEach(() => {
  document.body.innerHTML = '<div id="app"></div>';
  render = runInNewContext(`${script}\nrenderWeather;`, {
    document,
    window: { parent: { postMessage() {} }, addEventListener() {}, removeEventListener() {} },
  });
});

describe("weather card untrusted tool results", () => {
  it.each(["location", "condition", "precipitation", "sunrise", "sunset"])(
    "renders markup in %s as text", (field) => {
      const payload = '<img src=x onerror="alert(1)">';
      render({ location: "Nairobi", temperature: 20, [field]: payload });
      expect(document.querySelector("img")).toBeNull();
      expect(document.getElementById("app")!.textContent).toContain(payload);
    },
  );
  it("preserves decimal precipitation and zero values", () => {
    render({ location: "Nairobi", temperature: 0, precipitation: 0.25 });
    expect(document.getElementById("app")!.textContent).toContain("0.25 mm");
    render({ location: "Nairobi", temperature: 0, precipitation: 0 });
    expect(document.getElementById("app")!.textContent).toContain("0 mm");
  });
});
