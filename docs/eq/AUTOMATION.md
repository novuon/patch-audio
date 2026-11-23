# EQ automation architecture

Manual controls, controller input, DJ transitions, performance recording,
timelines, and future AI suggestions should address one semantic parameter
layer keyed by stable instance ID, band ID, and parameter name. The engine
owns authoritative values; surfaces are clients. Human input takes priority
under Patch's existing control policy. Automation updates are prepared away
from the callback and applied as bounded ramps or prepared state changes.

Dynamic EQ is a later extension: threshold, ratio, attack, release, range,
and detector source belong to a per-band dynamics extension. Do not overload
static band parameters or make AI a processing dependency.

