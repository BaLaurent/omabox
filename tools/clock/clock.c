// omabox-clock.so: a clean box's wall clock pinned (`up --clean`, #252, NOTES finding 359).
//
// Preloaded into every dynamically linked process of a clean box by the box's own /etc/ld.so.preload
// (bin/omabox puts it in the box's /etc overlay; the host's /etc is never changed): the session, the
// shell, `omabox run` commands (which start with a clean environment, so LD_PRELOAD would not reach
// them), D-Bus-activated services. It reads the instant from INSTANT_FILE, which only a clean box has;
// without it, outside a box, and in omabox's own tools and helpers, every call goes to libc unchanged:
// what they stamp the host compares with its own clock (`events --since 30s`, `record`'s first frame
// and its marks). Those are the tools in OWN_TOOLS (however started) and what runs with
// OMABOX_REAL_CLOCK=1 in its environment (bin/omabox's on_box and box_sys, and what they start).
//
// INSTANT_FILE: "SECONDS BOOT_NS". CLOCK_REALTIME reads SECONDS when CLOCK_BOOTTIME read BOOT_NS (the
// box's start), and runs from there: a clock frozen at SECONDS stood bash's $SECONDS still, so a wait
// measured with it (Omarchy's scripts have them) never ended.
//
// What it changes: CLOCK_REALTIME and its relatives (REALTIME_COARSE, REALTIME_ALARM, TAI) as
// clock_gettime, gettimeofday, time, timespec_get and syscall() read them. CLOCK_MONOTONIC and BOOTTIME
// stay the kernel's, so timers keep working. An absolute deadline on a shifted clock handed to the
// kernel (clock_nanosleep, timerfd_settime and timer_settime with the ABSTIME flag,
// pthread_cond_timedwait on a condition left on CLOCK_REALTIME, sem_timedwait, the timed and clock
// locks and joins, futex by syscall()) is moved into the real clock: left as the app computed it, it is
// months before the real now and returns at once, a busy loop where the app waits in one. A function
// looked up by name (dlsym, dlvsym) is this library's too.
//
// What it cannot reach: a static binary, a Go program (both read the vDSO themselves), a raw syscall
// instruction, io_uring timeouts, 32-bit programs (the library is 64-bit; their ld.so says it skips
// it), Flatpak apps (their own /etc), and what the kernel stamps (file times, socket timestamps).
#define _GNU_SOURCE
#include <dlfcn.h>
#include <elf.h>
#include <errno.h>
#include <fcntl.h>
#include <link.h>
#include <linux/futex.h>
#include <mqueue.h>
#include <pthread.h>
#include <semaphore.h>
#include <signal.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <sys/timerfd.h>
#include <time.h>
#include <unistd.h>

#include "../common/box.h"

#define INSTANT_FILE "/opt/omabox/clock/instant"
#define OWN_TOOLS "/opt/omabox/bin/"
#define NS 1000000000LL

// 0: not read yet, 1: pinned, 2: left alone. Read once, by whichever call comes first (the
// constructor, or a call made by another library's constructor before it ran); reading twice at once
// writes the same values.
static int state;
static int64_t base_ns, boot0_ns;

static int (*real_clock_gettime)(clockid_t, struct timespec *);
static int (*real_gettimeofday)(struct timeval *, void *);
static time_t (*real_time)(time_t *);
static int (*real_timespec_get)(struct timespec *, int);
static int (*real_clock_nanosleep)(clockid_t, int, const struct timespec *, struct timespec *);
static int (*real_timerfd_create)(int, int);
static int (*real_timerfd_settime)(int, int, const struct itimerspec *, struct itimerspec *);
static int (*real_timer_create)(clockid_t, struct sigevent *, timer_t *);
static int (*real_timer_settime)(timer_t, int, const struct itimerspec *, struct itimerspec *);
static int (*real_cond_timedwait)(pthread_cond_t *, pthread_mutex_t *, const struct timespec *);
static int (*real_cond_timedwait_2_2_5)(void *, pthread_mutex_t *, const struct timespec *);
static int (*real_cond_clockwait)(pthread_cond_t *, pthread_mutex_t *, clockid_t, const struct timespec *);
static int (*real_mutex_timedlock)(pthread_mutex_t *, const struct timespec *);
static int (*real_mutex_clocklock)(pthread_mutex_t *, clockid_t, const struct timespec *);
static int (*real_rwlock_timedrdlock)(pthread_rwlock_t *, const struct timespec *);
static int (*real_rwlock_timedwrlock)(pthread_rwlock_t *, const struct timespec *);
static int (*real_rwlock_clockrdlock)(pthread_rwlock_t *, clockid_t, const struct timespec *);
static int (*real_rwlock_clockwrlock)(pthread_rwlock_t *, clockid_t, const struct timespec *);
static int (*real_sem_timedwait)(sem_t *, const struct timespec *);
static int (*real_sem_clockwait)(sem_t *, clockid_t, const struct timespec *);
static int (*real_timedjoin)(pthread_t, void **, const struct timespec *);
static int (*real_clockjoin)(pthread_t, void **, clockid_t, const struct timespec *);
static long (*real_syscall)(long, ...);
static ssize_t (*real_mq_timedreceive)(mqd_t, char *, size_t, unsigned *, const struct timespec *);
static int (*real_mq_timedsend)(mqd_t, const char *, size_t, unsigned, const struct timespec *);

#define NEXT(p, name) (p = real_dlsym ? real_dlsym(RTLD_NEXT, name) : NULL)

static int64_t ts_ns(const struct timespec *t) { return (int64_t)t->tv_sec * NS + t->tv_nsec; }
static struct timespec ns_ts(int64_t n) {
    struct timespec t = { n / NS, n % NS };
    if (t.tv_nsec < 0) { t.tv_nsec += NS; t.tv_sec--; }
    return t;
}

static int read_instant(void) {
    int fd = open(INSTANT_FILE, O_RDONLY | O_CLOEXEC);
    if (fd < 0) return 0;
    char buf[128];
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    close(fd);
    if (n <= 0) return 0;
    buf[n] = 0;
    char *e, *e2;
    long long sec = strtoll(buf, &e, 10), boot = strtoll(e, &e2, 10);
    if (e == buf || e2 == e || sec <= 0 || boot < 0) return 0;
    base_ns = (int64_t)sec * NS;
    boot0_ns = boot;
    return 1;
}

static void *(*real_dlsym)(void *, const char *);
static void *(*real_dlvsym)(void *, const char *, const char *);

// A symbol libc defines, read from its own dynamic symbol table (GNU hash): libc's dlsym and dlvsym,
// which this library's (below) hand each lookup to. Not by dlsym or dlvsym (those names are this
// library's here), nor dlopen, which allocates: jemalloc calls syscall() from its constructor, before
// this library's has run, and the allocation came back here (every Quickshell crashed).
struct found { const char *name; void *addr; };

static int find_in_libc(struct dl_phdr_info *info, size_t size, void *data) {
    (void)size;
    struct found *f = data;
    const char *slash = strrchr(info->dlpi_name, '/');
    if (strcmp(slash ? slash + 1 : info->dlpi_name, "libc.so.6") != 0) return 0;
    const ElfW(Dyn) *dyn = NULL;
    for (int i = 0; i < info->dlpi_phnum; i++)
        if (info->dlpi_phdr[i].p_type == PT_DYNAMIC) dyn = (const void *)(info->dlpi_addr + info->dlpi_phdr[i].p_vaddr);
    if (!dyn) return 1;
    const ElfW(Sym) *syms = NULL;
    const char *strs = NULL;
    const uint32_t *gh = NULL;
    for (const ElfW(Dyn) *d = dyn; d->d_tag != DT_NULL; d++) {
        // (ld.so has relocated these in place on most machines; not on all.)
        ElfW(Addr) a = d->d_un.d_ptr < info->dlpi_addr ? d->d_un.d_ptr + info->dlpi_addr : d->d_un.d_ptr;
        if (d->d_tag == DT_SYMTAB) syms = (const void *)a;
        else if (d->d_tag == DT_STRTAB) strs = (const void *)a;
        else if (d->d_tag == DT_GNU_HASH) gh = (const void *)a;
    }
    if (!syms || !strs || !gh) return 1;
    uint32_t nbuckets = gh[0], first = gh[1], nbloom = gh[2];
    const uint32_t *buckets = (const uint32_t *)((const ElfW(Addr) *)(gh + 4) + nbloom);
    const uint32_t *chain = buckets + nbuckets;
    uint32_t hash = 5381;
    for (const unsigned char *c = (const unsigned char *)f->name; *c; c++) hash = hash * 33 + *c;
    uint32_t i = buckets[hash % nbuckets];
    if (i < first) return 1;
    for (;; i++) {
        uint32_t h2 = chain[i - first];
        if ((hash | 1) == (h2 | 1) && syms[i].st_shndx != SHN_UNDEF && strcmp(strs + syms[i].st_name, f->name) == 0) {
            f->addr = (void *)(info->dlpi_addr + syms[i].st_value);
            return 1;
        }
        if (h2 & 1) return 1;
    }
}

static void *libc_symbol(const char *name) {
    struct found f = { name, NULL };
    dl_iterate_phdr(find_in_libc, &f);
    return f.addr;
}

static void resolve_dl(void) {
    if (real_dlsym && real_dlvsym) return;
    real_dlsym = libc_symbol("dlsym");
    real_dlvsym = libc_symbol("dlvsym");
}

static void init(void) {
    // (Once per thread at a time: what init calls must not come back into it.)
    static __thread int busy __attribute__((tls_model("initial-exec")));
    if (__atomic_load_n(&state, __ATOMIC_ACQUIRE) || busy) return;
    busy = 1;
    resolve_dl();
    NEXT(real_syscall, "syscall");
    NEXT(real_clock_gettime, "clock_gettime");
    NEXT(real_gettimeofday, "gettimeofday");
    NEXT(real_time, "time");
    NEXT(real_timespec_get, "timespec_get");
    NEXT(real_clock_nanosleep, "clock_nanosleep");
    NEXT(real_timerfd_create, "timerfd_create");
    NEXT(real_timerfd_settime, "timerfd_settime");
    NEXT(real_timer_create, "timer_create");
    NEXT(real_timer_settime, "timer_settime");
    NEXT(real_cond_timedwait, "pthread_cond_timedwait");
    NEXT(real_cond_clockwait, "pthread_cond_clockwait");
    if (real_dlvsym) real_cond_timedwait_2_2_5 = real_dlvsym(RTLD_NEXT, "pthread_cond_timedwait", "GLIBC_2.2.5");
    NEXT(real_mutex_timedlock, "pthread_mutex_timedlock");
    NEXT(real_mutex_clocklock, "pthread_mutex_clocklock");
    NEXT(real_rwlock_timedrdlock, "pthread_rwlock_timedrdlock");
    NEXT(real_rwlock_timedwrlock, "pthread_rwlock_timedwrlock");
    NEXT(real_rwlock_clockrdlock, "pthread_rwlock_clockrdlock");
    NEXT(real_rwlock_clockwrlock, "pthread_rwlock_clockwrlock");
    NEXT(real_sem_timedwait, "sem_timedwait");
    NEXT(real_sem_clockwait, "sem_clockwait");
    NEXT(real_timedjoin, "pthread_timedjoin_np");
    NEXT(real_clockjoin, "pthread_clockjoin_np");
    NEXT(real_mq_timedreceive, "mq_timedreceive");
    NEXT(real_mq_timedsend, "mq_timedsend");
    const char *exe = (const char *)getauxval(AT_EXECFN);
    int own = (exe && strncmp(exe, OWN_TOOLS, strlen(OWN_TOOLS)) == 0) || getenv("OMABOX_REAL_CLOCK");
    __atomic_store_n(&state, real_clock_gettime && !own && omabox_inside_box() && read_instant() ? 1 : 2, __ATOMIC_RELEASE);
    busy = 0;
}

__attribute__((constructor)) static void start(void) { init(); }

static int shifted(clockid_t c) {
    init();
    return __atomic_load_n(&state, __ATOMIC_ACQUIRE) == 1 &&
           (c == CLOCK_REALTIME || c == CLOCK_REALTIME_COARSE || c == CLOCK_REALTIME_ALARM || c == CLOCK_TAI);
}

// The box's CLOCK_REALTIME now.
static int64_t fake_now(void) {
    struct timespec b;
    real_clock_gettime(CLOCK_BOOTTIME, &b);
    return base_ns + (ts_ns(&b) - boot0_ns);
}

// Shifted clock C as the box reads it (TAI: its whole seconds ahead of REALTIME, kept).
static int64_t fake(clockid_t c) {
    int64_t f = fake_now();
    if (c == CLOCK_TAI) {
        struct timespec tai, rt;
        real_clock_gettime(CLOCK_TAI, &tai);
        real_clock_gettime(CLOCK_REALTIME, &rt);
        f += (ts_ns(&tai) - ts_ns(&rt) + NS / 2) / NS * NS;
    }
    return f;
}

// An absolute deadline T on shifted clock C, as the kernel's clock C reads it: as far from its now
// as T is from the box's. One that means never (systemd's TIME_T_MAX, which tells it of a clock set:
// moved, it overflowed, fired at once, and the user manager looped) and one the kernel refuses stay
// as they are.
static struct timespec to_real(clockid_t c, const struct timespec *t) {
    if (t->tv_sec > INT64_MAX / NS / 2 || t->tv_sec < 0 || t->tv_nsec < 0 || t->tv_nsec >= NS) return *t;
    struct timespec r;
    real_clock_gettime(c == CLOCK_REALTIME_ALARM ? CLOCK_REALTIME : c, &r);
    return ns_ts(ts_ns(t) - fake(c) + ts_ns(&r));
}

// T, or BUF holding it moved into the kernel's clock when C is shifted.
static const struct timespec *moved(clockid_t c, const struct timespec *t, struct timespec *buf) {
    if (!t || !shifted(c)) return t;
    *buf = to_real(c, t);
    return buf;
}

int clock_gettime(clockid_t c, struct timespec *t) {
    if (!shifted(c)) {
        if (!real_clock_gettime) { errno = ENOSYS; return -1; }
        return real_clock_gettime(c, t);
    }
    if (!t) { errno = EFAULT; return -1; }
    *t = ns_ts(fake(c));
    return 0;
}

int gettimeofday(struct timeval *restrict tv, void *restrict tz) {
    if (!shifted(CLOCK_REALTIME) && real_gettimeofday) return real_gettimeofday(tv, tz);
    struct timespec t;
    if (clock_gettime(CLOCK_REALTIME, &t) != 0) return -1;
    if (tv) { tv->tv_sec = t.tv_sec; tv->tv_usec = t.tv_nsec / 1000; }
    if (tz) memset(tz, 0, sizeof(struct timezone));
    return 0;
}

time_t time(time_t *out) {
    if (!shifted(CLOCK_REALTIME) && real_time) return real_time(out);
    struct timespec t;
    if (clock_gettime(CLOCK_REALTIME, &t) != 0) return (time_t)-1;
    if (out) *out = t.tv_sec;
    return t.tv_sec;
}

int timespec_get(struct timespec *t, int base) {
    if (base != TIME_UTC) {
        init();
        return real_timespec_get ? real_timespec_get(t, base) : 0;
    }
    return clock_gettime(CLOCK_REALTIME, t) == 0 ? base : 0;
}

int clock_nanosleep(clockid_t c, int flags, const struct timespec *req, struct timespec *rem) {
    init();
    if (!real_clock_nanosleep) return ENOSYS;
    struct timespec b;
    return real_clock_nanosleep(c, flags, flags & TIMER_ABSTIME ? moved(c, req, &b) : req, rem);
}

// Each timerfd's and timer's clock, kept as they are made: timerfd_settime and timer_settime are not
// told it. A timerfd past the table (or one made by a dup) is read from /proc/self/fdinfo.
#define FDS 65536
static unsigned char fd_clock[FDS];   // clock + 1; 0: not seen

int timerfd_create(int c, int flags) {
    init();
    if (!real_timerfd_create) { errno = ENOSYS; return -1; }
    int fd = real_timerfd_create(c, flags);
    if (fd >= 0 && fd < FDS) fd_clock[fd] = (unsigned char)(c + 1);
    return fd;
}

static int fdinfo_clock(int fd) {
    char path[64], buf[512];
    snprintf(path, sizeof(path), "/proc/self/fdinfo/%d", fd);
    int f = open(path, O_RDONLY | O_CLOEXEC);
    if (f < 0) return -1;
    ssize_t n = read(f, buf, sizeof(buf) - 1);
    close(f);
    if (n <= 0) return -1;
    buf[n] = 0;
    char *p = strstr(buf, "clockid:");
    return p ? atoi(p + 8) : -1;
}

int timerfd_settime(int fd, int flags, const struct itimerspec *v, struct itimerspec *old) {
    init();
    if (!real_timerfd_settime) { errno = ENOSYS; return -1; }
    if ((flags & TFD_TIMER_ABSTIME) && v && (v->it_value.tv_sec || v->it_value.tv_nsec)) {
        int c = fd >= 0 && fd < FDS && fd_clock[fd] ? fd_clock[fd] - 1 : fdinfo_clock(fd);
        if (c >= 0 && shifted(c)) {
            struct itimerspec r = *v;
            r.it_value = to_real(c, &v->it_value);
            return real_timerfd_settime(fd, flags, &r, old);
        }
    }
    return real_timerfd_settime(fd, flags, v, old);
}

#define TIMERS 256
static struct { timer_t id; clockid_t c; } timers[TIMERS];
static pthread_mutex_t timers_lock = PTHREAD_MUTEX_INITIALIZER;

int timer_create(clockid_t c, struct sigevent *restrict sev, timer_t *restrict id) {
    init();
    if (!real_timer_create) { errno = ENOSYS; return -1; }
    int rc = real_timer_create(c, sev, id);
    // (Every timer's, so a deleted one's id given to a timer on another clock takes that clock.)
    if (rc == 0) {
        pthread_mutex_lock(&timers_lock);
        for (int i = 0; i < TIMERS; i++)
            if (!timers[i].id || timers[i].id == *id) { timers[i].id = *id; timers[i].c = c; break; }
        pthread_mutex_unlock(&timers_lock);
    }
    return rc;
}

int timer_settime(timer_t id, int flags, const struct itimerspec *restrict v, struct itimerspec *restrict old) {
    init();
    if (!real_timer_settime) { errno = ENOSYS; return -1; }
    if ((flags & TIMER_ABSTIME) && v && (v->it_value.tv_sec || v->it_value.tv_nsec)) {
        clockid_t c = -1;
        pthread_mutex_lock(&timers_lock);
        for (int i = 0; i < TIMERS && timers[i].id; i++)
            if (timers[i].id == id) { c = timers[i].c; break; }
        pthread_mutex_unlock(&timers_lock);
        if (c >= 0 && shifted(c)) {
            struct itimerspec r = *v;
            r.it_value = to_real(c, &v->it_value);
            return real_timer_settime(id, flags, &r, old);
        }
    }
    return real_timer_settime(id, flags, v, old);
}

// A condition's clock is glibc's __wrefs bit 1 (__PTHREAD_COND_CLOCK_MONOTONIC_MASK): set by
// pthread_condattr_setclock(CLOCK_MONOTONIC), as Qt, GLib and libstdc++'s steady waits use. Only one
// left on CLOCK_REALTIME has its deadline moved.
#define COND_MONOTONIC 2

int pthread_cond_timedwait(pthread_cond_t *restrict cv, pthread_mutex_t *restrict m, const struct timespec *restrict t) {
    init();
    if (!real_cond_timedwait) return ENOSYS;
    if (t && !(__atomic_load_n(&cv->__data.__wrefs, __ATOMIC_RELAXED) & COND_MONOTONIC) && shifted(CLOCK_REALTIME)) {
        struct timespec r = to_real(CLOCK_REALTIME, t);
        return real_cond_timedwait(cv, m, &r);
    }
    return real_cond_timedwait(cv, m, t);
}

// glibc 2.2.5's pthread_cond_timedwait, for the conditions its pthread_cond_init of that version
// makes, which are always on CLOCK_REALTIME. NVIDIA's driver libraries look up their pthread functions
// by that version (dlvsym), not the current one.
static int cond_timedwait_2_2_5(void *cv, pthread_mutex_t *m, const struct timespec *t) {
    init();
    if (!real_cond_timedwait_2_2_5) return ENOSYS;
    struct timespec b;
    return real_cond_timedwait_2_2_5(cv, m, moved(CLOCK_REALTIME, t, &b));
}

// The rest take their clock as an argument (the clock ones) or always wait on CLOCK_REALTIME.
#define REAL(p, fail) do { init(); if (!(p)) { fail; } } while (0)
#define NOSYS return ENOSYS
#define NOSYS_ERRNO errno = ENOSYS; return -1

int pthread_cond_clockwait(pthread_cond_t *restrict cv, pthread_mutex_t *restrict m, clockid_t c, const struct timespec *restrict t) {
    REAL(real_cond_clockwait, NOSYS);
    struct timespec b;
    return real_cond_clockwait(cv, m, c, moved(c, t, &b));
}
int pthread_mutex_timedlock(pthread_mutex_t *restrict m, const struct timespec *restrict t) {
    REAL(real_mutex_timedlock, NOSYS);
    struct timespec b;
    return real_mutex_timedlock(m, moved(CLOCK_REALTIME, t, &b));
}
int pthread_mutex_clocklock(pthread_mutex_t *restrict m, clockid_t c, const struct timespec *restrict t) {
    REAL(real_mutex_clocklock, NOSYS);
    struct timespec b;
    return real_mutex_clocklock(m, c, moved(c, t, &b));
}
int pthread_rwlock_timedrdlock(pthread_rwlock_t *restrict l, const struct timespec *restrict t) {
    REAL(real_rwlock_timedrdlock, NOSYS);
    struct timespec b;
    return real_rwlock_timedrdlock(l, moved(CLOCK_REALTIME, t, &b));
}
int pthread_rwlock_timedwrlock(pthread_rwlock_t *restrict l, const struct timespec *restrict t) {
    REAL(real_rwlock_timedwrlock, NOSYS);
    struct timespec b;
    return real_rwlock_timedwrlock(l, moved(CLOCK_REALTIME, t, &b));
}
int pthread_rwlock_clockrdlock(pthread_rwlock_t *restrict l, clockid_t c, const struct timespec *restrict t) {
    REAL(real_rwlock_clockrdlock, NOSYS);
    struct timespec b;
    return real_rwlock_clockrdlock(l, c, moved(c, t, &b));
}
int pthread_rwlock_clockwrlock(pthread_rwlock_t *restrict l, clockid_t c, const struct timespec *restrict t) {
    REAL(real_rwlock_clockwrlock, NOSYS);
    struct timespec b;
    return real_rwlock_clockwrlock(l, c, moved(c, t, &b));
}
int sem_timedwait(sem_t *restrict s, const struct timespec *restrict t) {
    REAL(real_sem_timedwait, NOSYS_ERRNO);
    struct timespec b;
    return real_sem_timedwait(s, moved(CLOCK_REALTIME, t, &b));
}
int sem_clockwait(sem_t *restrict s, clockid_t c, const struct timespec *restrict t) {
    REAL(real_sem_clockwait, NOSYS_ERRNO);
    struct timespec b;
    return real_sem_clockwait(s, c, moved(c, t, &b));
}
int pthread_timedjoin_np(pthread_t th, void **ret, const struct timespec *t) {
    REAL(real_timedjoin, NOSYS);
    struct timespec b;
    return real_timedjoin(th, ret, moved(CLOCK_REALTIME, t, &b));
}
int pthread_clockjoin_np(pthread_t th, void **ret, clockid_t c, const struct timespec *t) {
    REAL(real_clockjoin, NOSYS);
    struct timespec b;
    return real_clockjoin(th, ret, c, moved(c, t, &b));
}
ssize_t mq_timedreceive(mqd_t q, char *restrict msg, size_t len, unsigned *restrict prio, const struct timespec *restrict t) {
    REAL(real_mq_timedreceive, NOSYS_ERRNO);
    struct timespec b;
    return real_mq_timedreceive(q, msg, len, prio, moved(CLOCK_REALTIME, t, &b));
}
int mq_timedsend(mqd_t q, const char *msg, size_t len, unsigned prio, const struct timespec *t) {
    REAL(real_mq_timedsend, NOSYS_ERRNO);
    struct timespec b;
    return real_mq_timedsend(q, msg, len, prio, moved(CLOCK_REALTIME, t, &b));
}

// syscall(2) by hand, as NVIDIA's driver libraries wait (futex) and some programs read the clock: the
// same calls as above, by number. (Six arguments read whatever the caller passed, as libc's own
// syscall does.)
long syscall(long n, ...) {
    va_list ap;
    va_start(ap, n);
    long a1 = va_arg(ap, long), a2 = va_arg(ap, long), a3 = va_arg(ap, long);
    long a4 = va_arg(ap, long), a5 = va_arg(ap, long), a6 = va_arg(ap, long);
    va_end(ap);
    init();
    if (!real_syscall) { errno = ENOSYS; return -1; }
    if (n == SYS_futex && a4 && shifted(CLOCK_REALTIME)) {
        int op = (int)a2, cmd = op & FUTEX_CMD_MASK;
        // Absolute deadlines: WAIT_BITSET and WAIT_REQUEUE_PI on the clock the flag names, LOCK_PI
        // always on CLOCK_REALTIME, LOCK_PI2 on it with the flag. (FUTEX_WAIT's timeout is relative.)
        int real_abs = cmd == FUTEX_LOCK_PI ||
                       ((op & FUTEX_CLOCK_REALTIME) &&
                        (cmd == FUTEX_WAIT_BITSET || cmd == FUTEX_WAIT_REQUEUE_PI || cmd == FUTEX_LOCK_PI2));
        if (real_abs) {
            struct timespec r = to_real(CLOCK_REALTIME, (const struct timespec *)a4);
            return real_syscall(n, a1, a2, a3, (long)&r, a5, a6);
        }
    } else if (n == SYS_clock_gettime && shifted((clockid_t)a1)) {
        return clock_gettime((clockid_t)a1, (struct timespec *)a2);
    } else if (n == SYS_gettimeofday && shifted(CLOCK_REALTIME)) {
        return gettimeofday((struct timeval *)a1, (void *)a2);
    } else if (n == SYS_time && shifted(CLOCK_REALTIME)) {
        return time((time_t *)a1);
    } else if (n == SYS_clock_nanosleep && ((int)a2 & TIMER_ABSTIME) && a3 && shifted((clockid_t)a1)) {
        struct timespec r = to_real((clockid_t)a1, (const struct timespec *)a3);
        return real_syscall(n, a1, a2, (long)&r, a4, a5, a6);
    }
    return real_syscall(n, a1, a2, a3, a4, a5, a6);
}

// dlsym and dlvsym: a library that looks libc's functions up by name gets this library's in their
// place, as one that links them does. NVIDIA's driver libraries dlopen libc and look up
// pthread_cond_timedwait (glibc 2.2.5's, with dlvsym), and waited with libc's own on deadlines made
// from the box's gettimeofday: each returned at once, four threads of a GTK 4 app at 100% (finding
// 359). Only a lookup that found libc's function (the one RTLD_NEXT found here) is swapped. RTLD_NEXT
// is passed on as a tail call, so libc sees the caller's own place, not this library's.
static const struct { const char *name; void **real; void *own; } swaps[] = {
#define SWAP(n, r) { #n, (void **)&r, (void *)n }
    SWAP(clock_gettime, real_clock_gettime), SWAP(clock_nanosleep, real_clock_nanosleep),
    SWAP(timerfd_create, real_timerfd_create), SWAP(timerfd_settime, real_timerfd_settime),
    SWAP(timer_create, real_timer_create), SWAP(timer_settime, real_timer_settime),
    SWAP(pthread_cond_timedwait, real_cond_timedwait), SWAP(pthread_cond_clockwait, real_cond_clockwait),
    SWAP(pthread_mutex_timedlock, real_mutex_timedlock), SWAP(pthread_mutex_clocklock, real_mutex_clocklock),
    SWAP(pthread_rwlock_timedrdlock, real_rwlock_timedrdlock), SWAP(pthread_rwlock_timedwrlock, real_rwlock_timedwrlock),
    SWAP(pthread_rwlock_clockrdlock, real_rwlock_clockrdlock), SWAP(pthread_rwlock_clockwrlock, real_rwlock_clockwrlock),
    SWAP(sem_timedwait, real_sem_timedwait), SWAP(sem_clockwait, real_sem_clockwait),
    SWAP(pthread_timedjoin_np, real_timedjoin), SWAP(pthread_clockjoin_np, real_clockjoin),
    SWAP(mq_timedreceive, real_mq_timedreceive), SWAP(mq_timedsend, real_mq_timedsend),
    SWAP(syscall, real_syscall), SWAP(gettimeofday, real_gettimeofday), SWAP(time, real_time),
    SWAP(timespec_get, real_timespec_get),
    { "pthread_cond_timedwait", (void **)&real_cond_timedwait_2_2_5, (void *)cond_timedwait_2_2_5 },
#undef SWAP
};

// R, the function NAME found, or this library's in its place when R is libc's.
static void *own_instead(const char *name, void *r) {
    if (!r || !name || __atomic_load_n(&state, __ATOMIC_ACQUIRE) != 1) return r;
    for (unsigned i = 0; i < sizeof(swaps) / sizeof(*swaps); i++)
        if (r == *swaps[i].real && strcmp(name, swaps[i].name) == 0) return swaps[i].own;
    return r;
}

void *dlsym(void *restrict h, const char *restrict name) {
    resolve_dl();
    if (!real_dlsym) return NULL;
    if (h == RTLD_NEXT) __attribute__((musttail)) return real_dlsym(h, name);
    return own_instead(name, real_dlsym(h, name));
}

void *dlvsym(void *restrict h, const char *restrict name, const char *restrict version) {
    resolve_dl();
    if (!real_dlvsym) return NULL;
    if (h == RTLD_NEXT) __attribute__((musttail)) return real_dlvsym(h, name, version);
    return own_instead(name, real_dlvsym(h, name, version));
}
