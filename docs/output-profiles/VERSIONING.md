# Versioning

`format_version` is a positive integer. Unknown major formats fail closed to flat audio. Additive optional fields may be introduced compatibly; semantic changes to identity, coefficients, provenance, or safety constraints require a new format version. Validators reject unsupported versions, oversized filter lists, invalid sample rates, unstable/non-finite coefficients, excessive boosts, invalid IDs, and malformed data before audio publication.
