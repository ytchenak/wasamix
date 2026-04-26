# Security Policy

## Supported versions

Only the latest released version receives fixes. wasamix is a small single-binary tray app with no network surface; security fixes are expected to be rare.

## Reporting a vulnerability

**Do not open a public GitHub issue for security reports.**

Instead, use GitHub's [private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability) on this repository (Security → Advisories → "Report a vulnerability"). If that's unavailable in your mirror, open an issue titled `SECURITY — please contact privately` without details and a maintainer will reach out.

Please include:

- A description of the issue and its impact
- Steps to reproduce, or a proof-of-concept
- The version / commit you tested against
- Whether you'd like public credit once a fix ships

You can expect an initial acknowledgement within a week. Fix timelines depend on severity and complexity — we'll keep you posted.

## Scope

In scope: anything that lets a local process escalate privileges, crash the app in a way that corrupts unrelated audio state, or exfiltrate audio to an unauthorized destination.

Out of scope: issues inherent to VB-Audio Virtual Cable itself (report those to VB-Audio), or attacks that already require admin rights on the machine.
