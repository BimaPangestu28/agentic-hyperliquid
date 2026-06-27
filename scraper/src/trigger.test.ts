import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { createWake } from "./trigger.js";

describe("createWake", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("resolves after the timeout when never fired", async () => {
    const wake = createWake();
    let done = false;
    const waiting = wake.wait(1000).then(() => { done = true; });
    await vi.advanceTimersByTimeAsync(999);
    expect(done).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    await waiting;
    expect(done).toBe(true);
  });

  it("resolves early when fire() is called during a wait", async () => {
    const wake = createWake();
    let done = false;
    const waiting = wake.wait(60_000).then(() => { done = true; });
    wake.fire();
    await waiting;
    expect(done).toBe(true);
  });

  it("remembers a fire() that lands between waits (no missed trigger)", async () => {
    const wake = createWake();
    wake.fire(); // fired while nothing is waiting (e.g. during a running cycle)
    let done = false;
    await wake.wait(60_000).then(() => { done = true; }); // returns immediately
    expect(done).toBe(true);
  });

  it("only consumes one pending fire", async () => {
    const wake = createWake();
    wake.fire();
    await wake.wait(60_000); // consumes the pending trigger
    let done = false;
    wake.wait(1000).then(() => { done = true; });
    expect(done).toBe(false); // second wait is a normal timeout again
    await vi.advanceTimersByTimeAsync(1000);
  });
});
