# Security Policy

## Supported Versions

Use this section to tell people about currently supported versions of your project.

| Version | Supported          |
|---------|--------------------|
| latest  | :white_check_mark: |

## Reporting a Vulnerability

If you discover a security vulnerability in this project, please report it responsibly by emailing **security@lumina.dev**.

### What to Include

- Type of vulnerability
- Location (contract name, function, line number or description)
- Steps to reproduce
- Potential impact
- Any mitigations you've discovered

### Expected Response Time

- **Initial acknowledgment**: Within 48 hours
- **Triage and severity assessment**: Within 7 days
- **Fix development**: Within 30 days for critical/high severity
- **Coordinate disclosure**: We will work with you to determine an appropriate timeline for public disclosure

### What We Consider In-Scope

- Smart contract vulnerabilities that could lead to fund loss
- Access control issues
- Logic bugs affecting staking, staking token changes, or treasury management
- Reentrancy issues
- Integer overflow/underflow
- Problems with the governance proposal flow

### Out of Scope

- CSS/JS vulnerabilities in front-end tools
- Social engineering or phishing
- Issues in external dependencies not maintained by this project
- Denial-of-service attacks requiring network-level control
- Issues requiring fund movement by the reporter

### Secure Communication

- **Encryption**: PGP key available at `security@lumina.dev` public key fingerprint: `0xABCD1234`
- **Preferred methods**: Email to `security@lumina.dev` or GitHub Security Advisory
- **Response channel**: We will respond via the same method you used to report

### Bug Bounty

Currently, this project does not offer financial rewards for vulnerability reports. However, we will:
- Acknowledge contributions in the project's release notes (with permission)
- Grant repo contributor status for significant findings
- Provide early access to new features for responsible reporters

### Preferred Disclosure Path

1. **Email** `security@lumina.dev` with "[SECURITY]" in the subject
2. **Wait for acknowledgment** within 48 hours
3. **Coordinate** on disclosure timeline
4. **Receive** credit in release notes (unless you prefer anonymity)

### Verification

We encourage reporters to verify their findings by:
- Running the existing test suite
- Checking the on-chain behavior with a small test
- Documenting the exact conditions under which the vulnerability manifests

### History

| Date | Description |
|------|-------------|
| 2026-09-29 | Initial security policy established |

## Scope

This security policy applies to all code within the `lumina-contracts` repository, including:

- `registry/src/lib.rs` — the main Lumina Registry contract
- `registry-v2/src/lib.rs` — the v2 registry implementation
- All test files and deployment scripts

This does not apply to:
- Forked or mirrored repositories
- Downstream distributions that modify the code
- Off-chain tools or front-ends integrating with the contracts