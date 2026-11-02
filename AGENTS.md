# Project instructions

## Communication

- Use ASD-STE100 when addressing the user. Use it from memory. Do not look it up.

## Code quality

- Keep Rust code idiomatic, clean, and minimal.
- Keep complexity low. Use clear names, early returns, and shallow control flow.
- Each function must serve one clear purpose.
- Make control functions declarative. Express one flow through named operations. Keep calculations, state changes, and I/O in focused functions.
- Keep state and behavior with their owner. Add types and modules only when they establish a clear responsibility.
- Prefer duplicate code when callers can have different requirements. Share a helper only when every caller requires its complete behavior.
- Do not extend a shared helper for only some callers. Keep caller-specific behavior local.
- Avoid speculative abstractions and unnecessary configuration options.
- Use proper icons. Do not use text characters to imitate icons.

## Verification and review

- Do not write or run tests.
- Use source review and relevant compilation, lint, and formatting checks.
- After making changes, perform an adversarial review of the full change before reporting completion. Include added files.
- Assume the change contains a defect. Examine failure paths, state transitions, compatibility, and unintended behavior changes.
- Look for unnecessary complexity, mixed responsibilities, and shared helpers with different caller requirements.
- Correct defects introduced by the change. Review the corrections again. Report any unresolved findings.
