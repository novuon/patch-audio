# Device matching

Stable identity uses backend and hardware metadata, not a human readable sink label. Useful inputs include PipeWire node/device properties, ALSA card and path identifiers, PCI/USB vendor/product/serial fields, Bluetooth address plus codec/mode, channel geometry, and display EDID for monitor audio. Omit unavailable identifiers. Names are aliases for the UI only.

A stable ID is a deterministic fingerprint of normalized, non-display hardware identifiers. Never fingerprint volatile sink indices, current rate, or user-visible aliases. Exact identity is the automatic-match baseline; weaker matches require explicit user confirmation and visibly lower confidence. Composite output chains list their hardware components in order.

On output change, identify and validate away from the RT callback, then publish a compiled state at a block boundary. A mismatch, parser error, profile absence, or service failure leaves playback on the flat path.
