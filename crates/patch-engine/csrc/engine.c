/*
 * Patch engine shim: one PipeWire filter node, "Patch Engine"
 * (node.name patch.engine), with a stereo input pair per named channel
 * (in_<name>_FL / in_<name>_FR), a stereo program output (out_FL/out_FR),
 * the same program before the system volume for recording (program_FL/FR),
 * a stereo headphone cue output (cue_FL/cue_FR) and a mono measurement
 * input (in_measure: a microphone, linked only while latency is measured).
 *
 * All DSP happens in Rust/assembly in `process`, called on PipeWire's
 * real-time data thread once per graph cycle together with the graph clock
 * (sample position, monotonic nsec, rate), so everything the engine
 * measures is stamped with the block it came from.
 *
 * This is the only C in Patch: the smallest surface needed to be a native,
 * in-graph node (zero added latency, sample-exact with the graph clock).
 */
#include <pipewire/pipewire.h>
#include <pipewire/filter.h>
#include <pipewire/impl-module.h>
#include <spa/node/io.h>
#include <spa/param/props.h>
#include <spa/param/audio/raw.h>
#include <spa/pod/iter.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <time.h>

#define MAX_CH 64
/* Output ports: see `struct engine`. Rust indexes them by number. */
#define OUT_PORTS 19

struct patch_clock {
    uint64_t position;
    uint64_t nsec;
    uint32_t rate;
    uint32_t frames;
    /* The driver's delay between position and hardware (samples): the
     * output device's buffer, the latency to the DAC as it is now. */
    int64_t delay;
    /* SPA_IO_CLOCK_FLAG_* of this cycle (XRUN_RECOVER: this node missed
     * the last cycle's deadline), and the driver's accumulated xrun time
     * (samples at the clock's rate; 0 from a driver that keeps none). */
    uint32_t flags;
    uint64_t xrun;
};

typedef void (*patch_process_fn)(void *user, const float **ins, float **outs, const struct patch_clock *clock);
/* The watched sink's volume: linear per side, and mute. */
typedef void (*patch_volume_fn)(void *user, float left, float right, int mute);

struct engine {
    struct pw_main_loop *loop;
    struct pw_context *context;
    struct pw_core *core;
    struct pw_filter *filter;
    struct spa_hook filter_listener;
    void *in_ports[MAX_CH * 2 + 1];
    /* out_FL/FR, cue_FL/FR, then the surround and mono outputs (FC, LFE,
     * SL, SR, RL, RR, MONO), the input mix (capture_FL/FR), the program
     * for recording (program_FL/FR) and the Mixer's AUX 1 and 2 mixes
     * (aux1_FL/FR, aux2_FL/FR): filled only while linked. */
    void *out_ports[OUT_PORTS];
    uint32_t channels;
    patch_process_fn process;
    void *user;
    /* The system volume, straight from PipeWire: the sink named
     * `sink_name` is bound and its Props followed (see
     * patch_engine_watch_volume). */
    struct pw_registry *registry;
    struct spa_hook registry_listener;
    struct pw_proxy *sink;
    struct spa_hook sink_listener;
    uint32_t sink_id;
    char sink_name[64];
    patch_volume_fn volume;
    void *volume_user;
};

static void on_process(void *data, struct spa_io_position *position)
{
    struct engine *e = data;
    if (!position) return;
    struct patch_clock clock = {
        .position = position->clock.position,
        .nsec = position->clock.nsec,
        .rate = position->clock.rate.denom,
        .frames = (uint32_t)position->clock.duration,
        .delay = position->clock.delay,
        .flags = position->clock.flags,
        .xrun = position->clock.xrun,
    };
    const float *ins[MAX_CH * 2 + 1];
    float *outs[OUT_PORTS];
    /* ins[channels * 2] is the measurement input. */
    for (uint32_t i = 0; i < e->channels * 2 + 1; i++) {
        ins[i] = pw_filter_get_dsp_buffer(e->in_ports[i], clock.frames);
    }
    for (uint32_t i = 0; i < OUT_PORTS; i++) {
        outs[i] = pw_filter_get_dsp_buffer(e->out_ports[i], clock.frames);
    }
    /* The cue pair may be unlinked (NULL): Rust skips it then. */
    if (outs[0] && outs[1]) {
        e->process(e->user, ins, outs, &clock);
    }
}

static const struct pw_filter_events filter_events = {
    PW_VERSION_FILTER_EVENTS,
    .process = on_process,
};

static void *add_port(struct pw_filter *f, enum spa_direction dir, const char *name)
{
    return pw_filter_add_port(f, dir, PW_FILTER_PORT_FLAG_MAP_BUFFERS, sizeof(void *),
        pw_properties_new(PW_KEY_FORMAT_DSP, "32 bit float mono audio",
                          PW_KEY_PORT_NAME, name, NULL),
        NULL, 0);
}

/* Create the node with one stereo input pair per name in `names` (a
 * comma-separated list, e.g. "bus,deck-a,deck-b"). NULL on failure. */
void *patch_engine_new(const char *names, patch_process_fn process, void *user)
{
    struct engine *e = calloc(1, sizeof(*e));
    if (!e) return NULL;
    e->process = process;
    e->user = user;

    pw_init(NULL, NULL);
    e->loop = pw_main_loop_new(NULL);
    if (!e->loop) goto fail;
    /* Our own context, without client.conf's automatic real-time module
     * ("module.rt" = false): that one asks the desktop portal first, and
     * the portal answers with a real-time CPU limit of 0 us, so the
     * kernel killed the engine the moment its audio thread ran real-time.
     * Loaded here it asks rtkit directly (or uses the realtime group's
     * limits), which grants a working limit. */
    e->context = pw_context_new(pw_main_loop_get_loop(e->loop),
        pw_properties_new("module.rt", "false", NULL), 0);
    if (!e->context) goto fail;
    if (!pw_context_load_module(e->context, "libpipewire-module-rt",
            "{ rtportal.enabled = false }", NULL))
        fprintf(stderr, "patch engine: no real-time module (running without real-time)\n");
    e->core = pw_context_connect(e->context, NULL, 0);
    if (!e->core) goto fail;
    e->filter = pw_filter_new(e->core, "Patch Engine",
        pw_properties_new(
            PW_KEY_MEDIA_TYPE, "Audio",
            PW_KEY_MEDIA_CATEGORY, "Filter",
            PW_KEY_MEDIA_ROLE, "DSP",
            PW_KEY_NODE_NAME, "patch.engine",
            PW_KEY_NODE_DESCRIPTION, "Patch Engine",
            PW_KEY_NODE_ALWAYS_PROCESS, "true",
            /* Passive while nothing plays (no deck, no app on Patch's
             * sinks): links made then don't keep the graph, or an idle
             * sound card, running. The engine turns active (and links its
             * outputs again) the moment a deck plays or an app opens a
             * stream on Patch: passive, it isn't scheduled with the device
             * and an app would go unheard. See patch_engine_set_always. */
            PW_KEY_NODE_PASSIVE, "true",
            NULL));
    if (!e->filter) goto fail;
    pw_filter_add_listener(e->filter, &e->filter_listener, &filter_events, e);

    char *list = strdup(names);
    char *save = NULL;
    char port[96];
    for (char *n = strtok_r(list, ",", &save); n && e->channels < MAX_CH; n = strtok_r(NULL, ",", &save)) {
        snprintf(port, sizeof(port), "in_%s_FL", n);
        e->in_ports[e->channels * 2] = add_port(e->filter, PW_DIRECTION_INPUT, port);
        snprintf(port, sizeof(port), "in_%s_FR", n);
        e->in_ports[e->channels * 2 + 1] = add_port(e->filter, PW_DIRECTION_INPUT, port);
        e->channels++;
    }
    free(list);
    e->in_ports[e->channels * 2] = add_port(e->filter, PW_DIRECTION_INPUT, "in_measure");
    e->out_ports[0] = add_port(e->filter, PW_DIRECTION_OUTPUT, "out_FL");
    e->out_ports[1] = add_port(e->filter, PW_DIRECTION_OUTPUT, "out_FR");
    e->out_ports[2] = add_port(e->filter, PW_DIRECTION_OUTPUT, "cue_FL");
    e->out_ports[3] = add_port(e->filter, PW_DIRECTION_OUTPUT, "cue_FR");
    {
        const char *more[] = { "out_FC", "out_LFE", "out_SL", "out_SR", "out_RL", "out_RR", "out_MONO" };
        for (int i = 0; i < 7; i++)
            e->out_ports[4 + i] = add_port(e->filter, PW_DIRECTION_OUTPUT, more[i]);
    }
    e->out_ports[11] = add_port(e->filter, PW_DIRECTION_OUTPUT, "capture_FL");
    e->out_ports[12] = add_port(e->filter, PW_DIRECTION_OUTPUT, "capture_FR");
    e->out_ports[13] = add_port(e->filter, PW_DIRECTION_OUTPUT, "program_FL");
    e->out_ports[14] = add_port(e->filter, PW_DIRECTION_OUTPUT, "program_FR");
    e->out_ports[15] = add_port(e->filter, PW_DIRECTION_OUTPUT, "aux1_FL");
    e->out_ports[16] = add_port(e->filter, PW_DIRECTION_OUTPUT, "aux1_FR");
    e->out_ports[17] = add_port(e->filter, PW_DIRECTION_OUTPUT, "aux2_FL");
    e->out_ports[18] = add_port(e->filter, PW_DIRECTION_OUTPUT, "aux2_FR");
    if (e->channels == 0) goto fail;
    if (pw_filter_connect(e->filter, PW_FILTER_FLAG_RT_PROCESS, NULL, 0) < 0) goto fail;
    return e;
fail:
    if (e->filter) pw_filter_destroy(e->filter);
    if (e->core) pw_core_disconnect(e->core);
    if (e->context) pw_context_destroy(e->context);
    if (e->loop) pw_main_loop_destroy(e->loop);
    free(e);
    return NULL;
}

/* A Props change on the watched sink (or the first enumeration): the
 * volume as the bar and media keys set it, with no process in between. */
static void on_sink_param(void *data, int seq, uint32_t id, uint32_t index,
    uint32_t next, const struct spa_pod *param)
{
    struct engine *e = data;
    if (id != SPA_PARAM_Props || !param || !spa_pod_is_object_type(param, SPA_TYPE_OBJECT_Props))
        return;
    float vols[SPA_AUDIO_MAX_CHANNELS];
    uint32_t n = 0;
    int mute = -1;
    const struct spa_pod_prop *prop;
    SPA_POD_OBJECT_FOREACH((const struct spa_pod_object *)param, prop) {
        if (prop->key == SPA_PROP_channelVolumes) {
            n = spa_pod_copy_array(&prop->value, SPA_TYPE_Float, vols, SPA_AUDIO_MAX_CHANNELS);
        } else if (prop->key == SPA_PROP_mute) {
            bool m;
            if (spa_pod_get_bool(&prop->value, &m) == 0) mute = m;
        }
    }
    if (n == 0 || mute < 0) return;
    e->volume(e->volume_user, vols[0], n > 1 ? vols[1] : vols[0], mute);
}

static const struct pw_node_events sink_events = {
    PW_VERSION_NODE_EVENTS,
    .param = on_sink_param,
};

static void drop_sink(struct engine *e)
{
    if (!e->sink) return;
    spa_hook_remove(&e->sink_listener);
    pw_proxy_destroy(e->sink);
    e->sink = NULL;
    e->sink_id = SPA_ID_INVALID;
}

static void on_global(void *data, uint32_t id, uint32_t permissions,
    const char *type, uint32_t version, const struct spa_dict *props)
{
    struct engine *e = data;
    if (strcmp(type, PW_TYPE_INTERFACE_Node) != 0 || !props) return;
    const char *name = spa_dict_lookup(props, PW_KEY_NODE_NAME);
    const char *class = spa_dict_lookup(props, PW_KEY_MEDIA_CLASS);
    if (!name || strcmp(name, e->sink_name) != 0 || !class || strcmp(class, "Audio/Sink") != 0)
        return;
    /* A recreated sink: follow the newest. */
    drop_sink(e);
    e->sink = pw_registry_bind(e->registry, id, PW_TYPE_INTERFACE_Node, PW_VERSION_NODE, 0);
    if (!e->sink) return;
    e->sink_id = id;
    pw_proxy_add_object_listener(e->sink, &e->sink_listener, &sink_events, e);
    uint32_t ids[] = { SPA_PARAM_Props };
    pw_node_subscribe_params((struct pw_node *)e->sink, ids, 1);
}

static void on_global_remove(void *data, uint32_t id)
{
    struct engine *e = data;
    if (id == e->sink_id) drop_sink(e);
}

static const struct pw_registry_events registry_events = {
    PW_VERSION_REGISTRY_EVENTS,
    .global = on_global,
    .global_remove = on_global_remove,
};

/* Follow the volume of the sink named `sink` (Audio/Sink): `volume` is
 * called on the engine's main loop at every change, from the moment the
 * loop runs. Call before patch_engine_run, from the same thread. 0 on
 * success. */
int patch_engine_watch_volume(void *handle, const char *sink, patch_volume_fn volume, void *user)
{
    struct engine *e = handle;
    if (!e || e->registry) return -1;
    snprintf(e->sink_name, sizeof(e->sink_name), "%s", sink);
    e->volume = volume;
    e->volume_user = user;
    e->sink_id = SPA_ID_INVALID;
    e->registry = pw_core_get_registry(e->core, PW_VERSION_REGISTRY, 0);
    if (!e->registry) return -1;
    pw_registry_add_listener(e->registry, &e->registry_listener, &registry_events, e);
    return 0;
}

/* Run the main loop until patch_engine_quit(), then free everything. */
void patch_engine_run(void *handle)
{
    struct engine *e = handle;
    pw_main_loop_run(e->loop);
    drop_sink(e);
    if (e->registry) {
        spa_hook_remove(&e->registry_listener);
        pw_proxy_destroy((struct pw_proxy *)e->registry);
    }
    pw_filter_destroy(e->filter);
    pw_core_disconnect(e->core);
    pw_context_destroy(e->context);
    pw_main_loop_destroy(e->loop);
    pw_deinit();
    free(e);
}

static int do_quit(struct spa_loop *loop, bool async, uint32_t seq, const void *data, size_t size, void *user)
{
    pw_main_loop_quit(user);
    return 0;
}

/* Thread-safe: queues the quit on the engine's own loop. */
void patch_engine_quit(void *handle)
{
    struct engine *e = handle;
    if (e && e->loop) {
        pw_loop_invoke(pw_main_loop_get_loop(e->loop), do_quit, 0, NULL, 0, false, e->loop);
    }
}

struct rename { struct pw_filter *filter; char name[96]; };

static int do_rename(struct spa_loop *loop, bool async, uint32_t seq, const void *data, size_t size, void *user)
{
    const struct rename *r = data;
    struct spa_dict_item items[] = {
        SPA_DICT_ITEM_INIT(PW_KEY_NODE_NAME, r->name),
        SPA_DICT_ITEM_INIT(PW_KEY_NODE_DESCRIPTION, "Patch Engine (retiring)"),
    };
    struct spa_dict dict = SPA_DICT_INIT_ARRAY(items);
    pw_filter_update_properties(r->filter, NULL, &dict);
    return 0;
}

/* Thread-safe: rename the node (node.name) on the engine's own loop, so a
 * new engine can take "patch.engine" during a handover. */
void patch_engine_set_name(void *handle, const char *name)
{
    struct engine *e = handle;
    if (!e || !e->loop) return;
    struct rename r = { .filter = e->filter };
    snprintf(r.name, sizeof(r.name), "%s", name);
    pw_loop_invoke(pw_main_loop_get_loop(e->loop), do_rename, 0, &r, sizeof(r), true, NULL);
}

struct always { struct pw_filter *filter; int on; };

static int do_always(struct spa_loop *loop, bool async, uint32_t seq, const void *data, size_t size, void *user)
{
    const struct always *a = data;
    /* PipeWire fixes a link's passivity from its nodes' node.passive when
     * it's made: the caller makes the output links again after this. */
    struct spa_dict_item items[] = {
        SPA_DICT_ITEM_INIT(PW_KEY_NODE_ALWAYS_PROCESS, a->on ? "true" : "false"),
        SPA_DICT_ITEM_INIT(PW_KEY_NODE_PASSIVE, a->on ? "false" : "true"),
    };
    struct spa_dict dict = SPA_DICT_INIT_ARRAY(items);
    pw_filter_update_properties(a->filter, NULL, &dict);
    return 0;
}

/* Thread-safe: whether the graph runs for the engine alone (a deck plays).
 * Off, it runs only when something plays into Patch, and an idle device
 * suspends as it would without Patch. */
void patch_engine_set_always(void *handle, int on)
{
    struct engine *e = handle;
    if (!e || !e->loop) return;
    struct always a = { .filter = e->filter, .on = on };
    pw_loop_invoke(pw_main_loop_get_loop(e->loop), do_always, 0, &a, sizeof(a), true, NULL);
}

/* CLOCK_MONOTONIC in ns: the clock PipeWire stamps graph cycles with. */
uint64_t patch_engine_now_ns(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
}
