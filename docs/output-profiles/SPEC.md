# Patch Output Profile Format, version 1

Patch Tune is Patch's device aware output optimization and protection system. It is not an EQ preset manager. This specification describes portable device data; room calibration and listener preference remain separate records/layers.

Profiles are UTF-8 TOML data (`*.patch-output.toml`) parsed and validated before use. Files contain no executable content, plugin references, arbitrary paths, or code. Version 1 is local and offline; the repository is intended to remain open and community maintained.

## Shape

Required top level fields: `format_version = 1`, `id`, `device_type`, `identity`, `correction`, and `sample_rates`. `manufacturer`, `model`, `protection`, `room_response`, `listener_preference`, `latency_budget_ms`, and `notes` are optional. Unknown values are omitted; do not infer measurements from a product name.

`identity.stable_id` is required. Match it from normalized hardware properties (backend, bus, vendor/product IDs, serial, node name where stable, and channel layout); a display name alone is insufficient. `identity.components` represents a logical chain such as a USB DAC and active speakers. Component order is signal order. Profile matching must verify compatibility and may require a complete chain match.

`correction.filters` is an ordered list of up to eight RBJ peaking biquads. Each uses `frequency_hz`, `q`, and `gain_db`. Version 1 validates frequency against each declared sample rate, Q 0.1–20, and gain ±12 dB. Correction provenance is required and states `kind` (`measured`, `modeled`, `known`, `inferred`, or `user`), `source`, optional confidence (0…1), and conditions. Coefficients are regenerated for the active sample rate on a control thread.

`protection` holds a peak ceiling and maximum positive correction allowance only when supported by known profile data. Limits are not exact voice-coil temperature or excursion measurements. Omit unknown limits. Profiles without trustworthy safety data must use conservative generic output handling and must not claim device-specific protection.

`room_response` is optional room-specific data and must not be merged into portable device correction. `listener_preference` is optional and bounded separately; bypassing it cannot disable the safety stage. Headphone profiles use `device_type = "headphones"` and must not carry room correction as if the headphone response were a room.

## Example

```toml
format_version = 1
id = "example-usb-speaker-rev-a"
device_type = "speaker"
sample_rates = [44100, 48000, 96000]
latency_budget_ms = 5.0

[identity]
stable_id = "patch-output:0123456789abcdef"
bus = "usb"
vendor_id = "1234"
product_id = "abcd"
channel_layout = ["FL", "FR"]
components = []

[correction.provenance]
kind = "measured"
source = "community measurement set 2026-01"
confidence = 0.72
conditions = "on-axis, 0.5 m, calibrated microphone"

[[correction.filters]]
frequency_hz = 180.0
q = 0.8
gain_db = -2.0

[protection]
peak_ceiling_dbfs = -1.0
max_positive_eq_db = 0.0

[protection.provenance]
kind = "known"
source = "manufacturer published operating limit"
confidence = 0.8
```

The example is illustrative and does not describe a real product or validated profile.
