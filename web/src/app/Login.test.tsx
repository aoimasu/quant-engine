import { describe, it, expect, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { Login } from './Login';

describe('Login', () => {
  it('renders the brand lockup and a Sign in with Google button', () => {
    render(<Login onSignIn={() => {}} />);
    expect(screen.getByAltText(/quant engine/i)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /sign in with google/i })).toBeInTheDocument();
  });

  it('does not show the rejection callout by default', () => {
    render(<Login onSignIn={() => {}} />);
    expect(screen.queryByText(/access denied/i)).not.toBeInTheDocument();
  });

  it('shows the allowlist-rejection callout when rejected', () => {
    render(<Login rejected onSignIn={() => {}} />);
    expect(screen.getByText(/access denied/i)).toBeInTheDocument();
    expect(screen.getByText(/isn't on the admin allowlist/i)).toBeInTheDocument();
  });

  it('invokes the sign-in action on click', async () => {
    const onSignIn = vi.fn();
    render(<Login onSignIn={onSignIn} />);
    await userEvent.click(screen.getByRole('button', { name: /sign in with google/i }));
    expect(onSignIn).toHaveBeenCalledOnce();
  });
});
