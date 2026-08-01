import { Component, type ReactNode } from "react";

/**
 * Catches a render crash and shows it.
 *
 * Without this, a thrown error unmounts the whole React tree and the UI simply
 * *vanishes* — which is what a dialog disappearing on click looks like from the
 * outside, with no clue as to why. A visible message turns an inexplicable
 * disappearance into something reportable.
 */
export class ErrorBoundary extends Component<
  { children: ReactNode; label: string; onReset?: () => void },
  { error: Error | null }
> {
  state = { error: null as Error | null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error) {
    // Also logged, so it reaches the console even if the user dismisses it.
    console.error(`[rmux] ${this.props.label} crashed:`, error);
  }

  render() {
    if (!this.state.error) return this.props.children;

    return (
      <div className="grid h-full place-items-center p-6">
        <div className="window corner flex max-w-[460px] flex-col gap-3 p-5">
          <span className="kicker">{this.props.label} stopped</span>
          <p className="data text-[11px] leading-relaxed" style={{ color: "rgb(var(--primary))" }}>
            {this.state.error.message}
          </p>
          <button
            type="button"
            className="btn"
            onClick={() => {
              this.setState({ error: null });
              this.props.onReset?.();
            }}
          >
            Try again
          </button>
        </div>
      </div>
    );
  }
}
