/* X server stall monitor: is a latency tail the app's, or the desktop's?
 *
 * Every 1 ms for N seconds: a plain round trip (GetInputFocus) and one that
 * makes the server sync with the GPU (a 1x1 GetImage of the root, the same
 * kind of read latency_probe.rs uses). Prints every round trip over 10 ms and
 * a summary. Read-only: sends no input and changes nothing.
 *
 *   cc -O2 -o /tmp/xmon scripts/x-stall-monitor.c -lxcb && /tmp/xmon 90
 *
 * Run it with the app closed. If GetImage already stalls 10+ ms here, a p99
 * of that size in the probe is the X server's, not the terminal's
 * (COMPLETED.md, Phase 20, "The p99 tail").
 */
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>
#include <xcb/xcb.h>
static double now_ms(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec * 1e3 + t.tv_nsec / 1e6; }
int main(int argc, char **argv) {
    double secs = argc > 1 ? atof(argv[1]) : 60;
    xcb_connection_t *c = xcb_connect(NULL, NULL);
    if (xcb_connection_has_error(c)) { fprintf(stderr, "no display\n"); return 1; }
    xcb_window_t root = xcb_setup_roots_iterator(xcb_get_setup(c)).data->root;
    double start = now_ms(), end = start + secs * 1000;
    long n = 0, f5[2] = {0}, f10[2] = {0}; double mx[2] = {0};
    while (now_ms() < end) {
        for (int kind = 0; kind < 2; kind++) {
            double t = now_ms();
            if (kind == 0) free(xcb_get_input_focus_reply(c, xcb_get_input_focus(c), NULL));
            else free(xcb_get_image_reply(c, xcb_get_image(c, XCB_IMAGE_FORMAT_Z_PIXMAP, root, 0, 0, 1, 1, ~0u), NULL));
            double d = now_ms() - t;
            if (d > 5) f5[kind]++;
            if (d > 10) { f10[kind]++; printf("stall %s %.1f ms at t=%.2f s\n", kind ? "getimage" : "focus", d, (t - start) / 1e3); }
            if (d > mx[kind]) mx[kind] = d;
        }
        n++;
        usleep(1000);
    }
    printf("rounds %ld\nfocus:    >5ms %ld  >10ms %ld  max %.1f ms\ngetimage: >5ms %ld  >10ms %ld  max %.1f ms\n",
           n, f5[0], f10[0], mx[0], f5[1], f10[1], mx[1]);
    return 0;
}
