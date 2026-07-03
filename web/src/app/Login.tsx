import { Button, Callout } from '../design';
import { injectCss } from '../design/injectCss';
import { startLogin } from '../api/session';
import logoLockup from '../assets/logo-lockup.svg';

const CSS = `
.qe-login { min-height: 100vh; display: grid; place-items: center; padding: 24px;
  background:
    radial-gradient(60% 50% at 50% 0%, rgba(124,92,255,0.10), transparent 70%),
    var(--bg-app);
}
.qe-login__card { width: 100%; max-width: 400px; display: flex; flex-direction: column; gap: 22px;
  padding: 32px; background: var(--surface-card); border: 1px solid var(--border-subtle);
  border-radius: var(--radius-xl); box-shadow: var(--shadow-md), var(--highlight-top); }
.qe-login__brand { display: flex; flex-direction: column; align-items: center; gap: 14px; text-align: center; }
.qe-login__brand img { height: 44px; }
.qe-login__title { font-family: var(--font-display); font-size: var(--fs-h3); font-weight: 600; letter-spacing: var(--ls-tight); }
.qe-login__sub { font-size: var(--fs-sm); color: var(--text-tertiary); max-width: 30ch; line-height: 1.5; }
.qe-login__foot { text-align: center; font: 500 10px var(--font-mono); text-transform: uppercase; letter-spacing: .12em; color: var(--text-muted); }
.qe-google { width: 18px; height: 18px; flex: none; }
`;

injectCss('qe-login-css', CSS);

/** Google 'G' mark (inline so no external asset / CDN is needed). */
function GoogleMark() {
  return (
    <svg className="qe-google" viewBox="0 0 48 48" aria-hidden="true">
      <path fill="#EA4335" d="M24 9.5c3.54 0 6.71 1.22 9.21 3.6l6.85-6.85C35.9 2.38 30.47 0 24 0 14.62 0 6.51 5.38 2.56 13.22l7.98 6.19C12.43 13.72 17.74 9.5 24 9.5z" />
      <path fill="#4285F4" d="M46.98 24.55c0-1.57-.15-3.09-.38-4.55H24v9.02h12.94c-.58 2.96-2.26 5.48-4.78 7.18l7.73 6c4.51-4.18 7.09-10.36 7.09-17.65z" />
      <path fill="#FBBC05" d="M10.53 28.59c-.48-1.45-.76-2.99-.76-4.59s.27-3.14.76-4.59l-7.98-6.19C.92 16.46 0 20.12 0 24c0 3.88.92 7.54 2.56 10.78l7.97-6.19z" />
      <path fill="#34A853" d="M24 48c6.48 0 11.93-2.13 15.89-5.81l-7.73-6c-2.15 1.45-4.92 2.3-8.16 2.3-6.26 0-11.57-4.22-13.47-9.91l-7.98 6.19C6.51 42.62 14.62 48 24 48z" />
    </svg>
  );
}

export interface LoginProps {
  /** Show the allowlist-rejection state (valid Google login, not allowlisted). */
  rejected?: boolean;
  /** Override the sign-in action (tests). Defaults to a real OAuth redirect. */
  onSignIn?: () => void;
}

export function Login({ rejected = false, onSignIn = startLogin }: LoginProps) {
  return (
    <div className="qe-login">
      <div className="qe-login__card">
        <div className="qe-login__brand">
          <img src={logoLockup} alt="Quant Engine" />
          <div className="qe-login__title">Sign in to Quant Engine</div>
          <div className="qe-login__sub">
            Access is restricted to the engine's admin allowlist. Sign in with your Google account to
            continue.
          </div>
        </div>

        {rejected && (
          <Callout variant="danger" title="Access denied">
            Your Google account isn't on the admin allowlist for this engine. Ask an administrator to add
            your email, then try again.
          </Callout>
        )}

        <Button variant="secondary" size="lg" block onClick={onSignIn} iconLeft={<GoogleMark />}>
          Sign in with Google
        </Button>

        <div className="qe-login__foot">Systematic trading · Bounded risk</div>
      </div>
    </div>
  );
}
