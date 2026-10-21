/* Test-only LV2 passthrough. Lifecycle events prove unrelated stream changes
 * leave a running plugin instance intact. No file I/O in the audio callback. */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

typedef struct Descriptor Descriptor;
typedef struct { const char *uri; void *data; } Feature;
struct Descriptor {
    const char *uri;
    void *(*instantiate)(const Descriptor *, double, const char *, const Feature *const *);
    void (*connect)(void *, uint32_t, void *);
    void (*activate)(void *);
    void (*run)(void *, uint32_t);
    void (*deactivate)(void *);
    void (*cleanup)(void *);
    const void *(*extension)(const char *);
};
typedef struct { float *ports[4]; uint64_t frames; } State;

static void event(char value) {
    const char *path = getenv("ALIXER_TEST_PLUGIN_EVENTS");
    FILE *file = path ? fopen(path, "a") : NULL;
    if (file) { fputc(value, file); fclose(file); }
}
static void *instantiate(const Descriptor *d, double rate, const char *path, const Feature *const *features) {
    (void)d; (void)rate; (void)path; (void)features;
    State *state = calloc(1, sizeof(State));
    if (state) event('I');
    return state;
}
static void connect_port(void *handle, uint32_t port, void *data) {
    ((State *)handle)->ports[port] = data;
}
static void run(void *handle, uint32_t frames) {
    State *state = handle;
    for (uint32_t i = 0; i < frames; ++i) {
        state->ports[2][i] = state->ports[0][i];
        state->ports[3][i] = state->ports[1][i];
    }
    state->frames += frames;
}
static void cleanup(void *handle) { event('D'); free(handle); }
static const Descriptor descriptor = {
    "urn:alixer:test:continuity", instantiate, connect_port, NULL, run, NULL, cleanup, NULL
};
const Descriptor *lv2_descriptor(uint32_t index) { return index == 0 ? &descriptor : NULL; }
