/* eslint-disable react-refresh/only-export-components --
   A React error boundary must be a class component (the lifecycle hooks exist only there), and this
   file also holds its private fallback component; the react-refresh plugin only recognizes plain
   function-component exports. Fast-refresh ergonomics don't apply to this dep-free boundary. */
import { Component, type ErrorInfo, type ReactNode } from 'react';

export interface ErrorBoundaryProps {
  children: ReactNode;
  /**
   * When any entry here changes while an error is being shown, the boundary auto-resets (clears the
   * error and re-renders `children`). Use it to recover on context changes the user can't otherwise
   * act on — e.g. `App.tsx` passes the signed-in identity so an auth/session change clears a stale error.
   */
  resetKeys?: readonly unknown[];
  /**
   * Optional custom fallback. Receives a `reset` callback (clears the error) and the caught error.
   * Defaults to the built-in recoverable panel.
   */
  fallback?: (reset: () => void, error: Error) => ReactNode;
}

interface ErrorBoundaryState {
  error: Error | null;
}

/** Shallow element-wise comparison of two `resetKeys` arrays (either may be undefined). */
function keysChanged(a: readonly unknown[] | undefined, b: readonly unknown[] | undefined): boolean {
  if (a === b) return false;
  if (!a || !b || a.length !== b.length) return true;
  for (let i = 0; i < a.length; i += 1) {
    if (!Object.is(a[i], b[i])) return true;
  }
  return false;
}

/**
 * Dependency-free top-level React error boundary (QE-424). A render throw anywhere in `children` is
 * caught by {@link getDerivedStateFromError} and replaces the subtree with a **recoverable** fallback —
 * a `role="alert"` panel with a "Try again" control — instead of blanking the whole SPA. Recovery paths:
 *   1. the user clicks "Try again" ({@link reset} clears the error, re-rendering `children`), or
 *   2. a `resetKeys` entry changes (e.g. the signed-in identity flips), auto-clearing the error.
 *
 * A boundary must be a class component (React exposes the lifecycle only there); it is kept small and
 * self-contained (inline-styled fallback using CSS custom properties) so the fallback renders even when a
 * design primitive is the thing that threw.
 */
export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  state: ErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    // Diagnostics only — no telemetry dependency. Surfaces the component stack for debugging.
    console.error('ErrorBoundary caught a render error:', error, info.componentStack);
  }

  componentDidUpdate(prev: ErrorBoundaryProps): void {
    // Auto-recover when the reset context changes (e.g. an auth/session flip) while an error is shown.
    if (this.state.error && keysChanged(prev.resetKeys, this.props.resetKeys)) {
      this.reset();
    }
  }

  reset = (): void => {
    this.setState({ error: null });
  };

  render(): ReactNode {
    const { error } = this.state;
    if (error) {
      if (this.props.fallback) return this.props.fallback(this.reset, error);
      return <ErrorFallback reset={this.reset} />;
    }
    return this.props.children;
  }
}

/** The default recoverable fallback: an accessible "something went wrong" panel with a retry control. */
function ErrorFallback({ reset }: { reset: () => void }) {
  return (
    <div
      role="alert"
      style={{
        minHeight: '100vh',
        display: 'grid',
        placeItems: 'center',
        padding: 24,
        background: 'var(--bg-app)',
        color: 'var(--text-primary)',
        fontFamily: 'var(--font-sans)',
      }}
    >
      <div
        style={{
          maxWidth: 420,
          display: 'flex',
          flexDirection: 'column',
          gap: 12,
          textAlign: 'center',
          padding: 28,
          background: 'var(--surface-card)',
          border: '1px solid var(--border-subtle)',
          borderRadius: 'var(--radius-lg)',
        }}
      >
        <h1 style={{ fontFamily: 'var(--font-display)', fontSize: 18, fontWeight: 600 }}>
          Something went wrong
        </h1>
        <p style={{ fontSize: 14, color: 'var(--text-tertiary)' }}>
          This screen hit an unexpected error and couldn’t render. Your session is still active — try
          again, or switch to another screen.
        </p>
        <div>
          <button
            type="button"
            onClick={reset}
            style={{
              cursor: 'pointer',
              padding: '8px 16px',
              borderRadius: 'var(--radius-md)',
              border: '1px solid var(--border-default)',
              background: 'var(--accent-fill-soft)',
              color: 'var(--violet-200)',
              fontFamily: 'var(--font-sans)',
              fontSize: 13,
              fontWeight: 600,
            }}
          >
            Try again
          </button>
        </div>
      </div>
    </div>
  );
}
