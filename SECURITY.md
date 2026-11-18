# Security status

Development preview, not independently security-audited. Do not send secrets, prompts, account identifiers or model paths to public issues. Report suspected vulnerabilities privately to the repository owner.

Keep model files and host signing/authentication material outside the repository. Choose native explicitly; emulation is only a test backend. Existing host authorizer/privacy routing stays in place. Required future gates include bounded delivery isolation, stale handle protection, native cancellation/resource budgets, malicious model handling, model-license and device validation, and a security audit. Unit tests are not substitutes for these gates.
