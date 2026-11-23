# Correction model and signal order

Tune layers remain separate: device correction (portable measured/known device behavior), device protection (conservative constraints), room/environment correction (specific to physical space), psychoacoustic processing (optional), and listener preference (bounded subjective choices).

The engine insertion point is post channel mix and channel effects, before program peak measurement, program ring publication, and physical output. Within Tune the intended order is device correction → bounded room correction → optional psychoacoustic processing → preference → headroom allocation and final peak protection. Safety limits constrain every positive layer; a final guard is last. Room and headphone behavior are distinct. The current engine slice implements compiled biquads, ramped profile headroom and a zero-lookahead peak guard; other listed stages are architecture only.

RBJ coefficients are regenerated for the active rate. Version 1 profile coefficients are not copied between rates. Positive correction cost is explicit, headroom is attenuated smoothly, and profile validation precedes publication. IIR processing adds no algorithmic delay. Future FIR work must declare partition size and measured latency and use a proper partitioned convolution design for long responses.
