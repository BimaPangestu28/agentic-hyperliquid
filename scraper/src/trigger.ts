/**
 * A cancellable wait used to pace the polling loop. `wait(ms)` resolves after the timeout
 * OR immediately when `fire()` is called — letting an external signal (SIGUSR2) cut the
 * idle period short and force the next scan now. A `fire()` that lands between waits (while
 * a cycle is running) is remembered, so the following `wait()` returns at once and the
 * trigger is never missed.
 */
export interface Wake {
  /** Resolve after `ms`, or early if `fire()` is (or was) called. */
  wait(ms: number): Promise<void>;
  /** Cut the current wait short; if none is active, make the next `wait()` return at once. */
  fire(): void;
}

export function createWake(): Wake {
  let resolveCurrent: (() => void) | null = null;
  let pending = false;
  return {
    wait(ms: number): Promise<void> {
      if (pending) {
        pending = false;
        return Promise.resolve();
      }
      return new Promise<void>((resolve) => {
        const timer = setTimeout(() => {
          resolveCurrent = null;
          resolve();
        }, ms);
        resolveCurrent = () => {
          clearTimeout(timer);
          resolveCurrent = null;
          resolve();
        };
      });
    },
    fire(): void {
      if (resolveCurrent) resolveCurrent();
      else pending = true;
    },
  };
}
