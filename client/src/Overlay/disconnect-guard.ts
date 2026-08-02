import type { OverlayConnectionStatus } from "./connection";

/** 一時的な再接続では表示を維持しつつ、アプリ終了時の古い注釈を早く消す猶予。 */
export const OVERLAY_DISCONNECT_GRACE_MS = 3_000;

type TimerHandle = number;

export interface OverlayDisconnectGuardRuntime {
  setTimeout(callback: () => void, delayMs: number): TimerHandle;
  clearTimeout(handle: TimerHandle): void;
}

const browserRuntime: OverlayDisconnectGuardRuntime = {
  setTimeout: (callback, delayMs) => window.setTimeout(callback, delayMs),
  clearTimeout: (handle) => window.clearTimeout(handle),
};

/**
 * 切断がgraceを超えたときだけonExpiredを呼ぶ。
 * generationにより、再同期・破棄後に残った古いtimer callbackを無効化する。
 */
export class OverlayDisconnectGuard {
  private disconnected = false;
  private disposed = false;
  private generation = 0;
  private timer: TimerHandle | null = null;

  constructor(
    private onExpired: () => void,
    private runtime: OverlayDisconnectGuardRuntime = browserRuntime,
  ) {}

  update(status: OverlayConnectionStatus): void {
    if (this.disposed) return;
    if (status === "connected") {
      if (!this.disconnected) return;
      this.disconnected = false;
      this.generation++;
      this.clearTimer();
      return;
    }

    // reconnect試行が続いても、最初の切断からの猶予を延長しない。
    if (this.disconnected) return;
    this.disconnected = true;
    const generation = ++this.generation;
    this.timer = this.runtime.setTimeout(() => {
      if (
        this.disposed ||
        !this.disconnected ||
        this.generation !== generation
      ) {
        return;
      }
      this.timer = null;
      this.onExpired();
    }, OVERLAY_DISCONNECT_GRACE_MS);
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.disconnected = false;
    this.generation++;
    this.clearTimer();
  }

  private clearTimer(): void {
    if (this.timer !== null) this.runtime.clearTimeout(this.timer);
    this.timer = null;
  }
}
