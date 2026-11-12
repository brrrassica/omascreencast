/*
 * TLS key-log shim for M0-T5 capture.
 *
 * Spike tooling, not product code. It exists so a packet capture of a real
 * GameStream host can be decrypted: the host does not honour SSLKEYLOGFILE, and
 * the trace corpus is a capture fact that must not be invented (D1).
 *
 * How it works
 * ------------
 * Interpose `SSL_CTX_new` and, on every context the process creates, register
 * OpenSSL's own key-log callback. The callback appends the standard NSS
 * key-log line to the file named by SSLKEYLOGFILE. Nothing in the captured
 * process is modified, and no protocol byte is synthesised.
 *
 * This observes the *library* the oracle links. It does not read, copy, or
 * transcribe the oracle's source or any GPL implementation.
 *
 * Build:
 *   gcc -shared -fPIC -O2 -o sslkeylog.so sslkeylog.c -ldl
 *
 * Use:
 *   SSLKEYLOGFILE=/tmp/keys.log LD_PRELOAD=./sslkeylog.so sunshine ...
 */

#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>

#include <openssl/ssl.h>

static FILE *log_file;

static FILE *keylog_file(void) {
    if (log_file != NULL) {
        return log_file;
    }
    const char *path = getenv("SSLKEYLOGFILE");
    if (path == NULL || path[0] == '\0') {
        return NULL;
    }
    /* Append: several processes may share one capture run. */
    log_file = fopen(path, "a");
    return log_file;
}

static void keylog_callback(const SSL *ssl, const char *line) {
    (void)ssl;
    FILE *file = keylog_file();
    if (file == NULL || line == NULL) {
        return;
    }
    fputs(line, file);
    fputc('\n', file);
    fflush(file);
}

SSL_CTX *SSL_CTX_new(const SSL_METHOD *method) {
    static SSL_CTX *(*real_new)(const SSL_METHOD *);
    static void (*real_set_cb)(SSL_CTX *, SSL_CTX_keylog_cb_func);

    if (real_new == NULL) {
        real_new = (SSL_CTX * (*)(const SSL_METHOD *)) dlsym(RTLD_NEXT, "SSL_CTX_new");
    }
    if (real_new == NULL) {
        return NULL;
    }
    SSL_CTX *ctx = real_new(method);
    if (ctx == NULL) {
        return NULL;
    }
    if (real_set_cb == NULL) {
        real_set_cb =
            (void (*)(SSL_CTX *, SSL_CTX_keylog_cb_func)) dlsym(RTLD_NEXT,
                                                               "SSL_CTX_set_keylog_callback");
    }
    if (real_set_cb != NULL && keylog_file() != NULL) {
        real_set_cb(ctx, keylog_callback);
    }
    return ctx;
}
