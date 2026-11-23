# Profile security

Profiles are untrusted data. Parsing, validation, and coefficient compilation happen outside audio callbacks. Reject unsupported versions, malformed numbers, non-finite or unstable filters, excessive counts/gain, and invalid identifiers. Never interpret profile fields as paths, commands, plugins, or executable blobs. Community profile loading is local and offline by default. Generated machine kernels belong to Patch's verified kernel system and never inside a profile.
