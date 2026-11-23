// Signalsmith Stretch behind the calls the decks make of Rubber Band:
// feed any number of frames, then take what is available. Tester kits
// (PATCH_KIT) build with this instead of Rubber Band, which is GPL.
//
// Signalsmith is block in, block out: each step makes STEP output frames
// from STEP / ratio input frames. Input waits in a FIFO until a step has
// enough; output waits in another until it is retrieved. Both are sized
// once, so a running deck worker does not allocate.

#include "signalsmith-stretch.h"

#include <algorithm>
#include <cmath>
#include <cstring>
#include <vector>

namespace {

constexpr int STEP = 256;
// Largest feed (the decks ask for at most 2048 at a time) plus a step's input
// at the slowest speed (0.05×: 20 steps' worth), with room to spare.
constexpr int FIFO = 16384 + STEP * 24;

struct Fifo {
    std::vector<std::vector<float>> ch;
    int head = 0, len = 0;

    void init(int channels) { ch.assign(channels, std::vector<float>(FIFO)); }
    void clear() { head = len = 0; }
    // Compact so `n` more frames fit after the live ones.
    void make_room(int n) {
        if (head + len + n <= FIFO) return;
        for (auto &c : ch) std::memmove(c.data(), c.data() + head, sizeof(float) * len);
        head = 0;
    }
};

struct Patch {
    signalsmith::stretch::SignalsmithStretch<float> s{0x5eed};
    int channels;
    double ratio = 1.0;  // output time / input time
    double owed = 0.0;   // fractional input frames carried between steps
    Fifo in, out;
    std::vector<const float *> ip;
    std::vector<float *> op;

    Patch(int rate, int ch) : channels(ch), ip(ch), op(ch) {
        s.presetDefault(ch, float(rate));
        in.init(ch);
        out.init(ch);
    }

    int step_input() const {
        return int(std::floor(owed + STEP / ratio));
    }

    void run() {
        for (;;) {
            int need = step_input();
            if (need > in.len || out.head + out.len + STEP > FIFO) {
                if (need <= in.len && out.head > 0) {
                    out.make_room(STEP);
                    continue;
                }
                return;
            }
            for (int c = 0; c < channels; c++) {
                ip[c] = in.ch[c].data() + in.head;
                op[c] = out.ch[c].data() + out.head + out.len;
            }
            s.process(ip.data(), need, op.data(), STEP);
            owed += STEP / ratio - need;
            in.head += need;
            in.len -= need;
            out.len += STEP;
        }
    }
};

} // namespace

extern "C" {

void *patch_stretch_new(int rate, int channels) {
    return new Patch(rate, channels);
}

void patch_stretch_delete(void *p) { delete static_cast<Patch *>(p); }

void patch_stretch_reset(void *p) {
    auto *t = static_cast<Patch *>(p);
    t->s.reset();
    t->in.clear();
    t->out.clear();
    t->owed = 0.0;
}

void patch_stretch_set(void *p, double time_ratio, double pitch) {
    auto *t = static_cast<Patch *>(p);
    t->ratio = std::clamp(time_ratio, 0.25, 20.0);
    t->s.setTransposeFactor(float(pitch));
}

// After a reset Signalsmith's processing time is `inputLatency` input frames
// before the first frame fed, and output comes `outputLatency` frames after
// the processing time. The pad is fed as audio (the frames before the wanted
// position, so its analysis has real context), then the wanted position
// comes out after the pad and the look-ahead at the output's rate, plus the
// output latency.
int patch_stretch_start_pad(void *p) {
    return static_cast<Patch *>(p)->s.inputLatency();
}

int patch_stretch_start_delay(void *p) {
    auto *t = static_cast<Patch *>(p);
    return t->s.outputLatency() + int(std::lround(2.0 * t->s.inputLatency() * t->ratio));
}

int patch_stretch_required(void *p) {
    auto *t = static_cast<Patch *>(p);
    return std::max(t->step_input() - t->in.len, 0);
}

void patch_stretch_process(void *p, const float *const *input, int n) {
    auto *t = static_cast<Patch *>(p);
    int at = 0;
    while (at < n) {
        int k = std::min(n - at, FIFO / 2);
        t->in.make_room(k);
        if (t->in.head + t->in.len + k > FIFO) k = FIFO - t->in.head - t->in.len;
        if (k <= 0) break; // never: callers feed what `required` asks
        for (int c = 0; c < t->channels; c++)
            std::memcpy(t->in.ch[c].data() + t->in.head + t->in.len, input[c] + at, sizeof(float) * k);
        t->in.len += k;
        at += k;
        t->run();
    }
}

int patch_stretch_available(void *p) { return static_cast<Patch *>(p)->out.len; }

int patch_stretch_retrieve(void *p, float *const *output, int n) {
    auto *t = static_cast<Patch *>(p);
    int k = std::min(n, t->out.len);
    for (int c = 0; c < t->channels; c++)
        std::memcpy(output[c], t->out.ch[c].data() + t->out.head, sizeof(float) * k);
    t->out.head += k;
    t->out.len -= k;
    if (t->out.len == 0) t->out.head = 0;
    t->run(); // output room freed: finish steps the input already allows
    return k;
}

} // extern "C"
