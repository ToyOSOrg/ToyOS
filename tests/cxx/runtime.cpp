// What `cxx_runtime` compiles with the toolchain's clang and runs on ToyOS:
// libc++'s containers, strings and streams, exceptions through frames with
// destructors, threads with their thread_local and static destructors, and
// libc's threads, keys, mutexes, condition variables and exit handlers. Every
// line it prints is the same on any host but the one that names `EPERM`,
// which is POSIX's answer and not every host's.
#include <atomic>
#include <cerrno>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <cstdio>
#include <ctime>
#include <exception>
#include <iostream>
#include <map>
#include <mutex>
#include <numeric>
#include <pthread.h>
#include <sstream>
#include <stdexcept>
#include <string>
#include <sys/random.h>
#include <thread>
#include <utility>
#include <vector>

namespace {

std::atomic<int> thread_locals_destroyed{0};
int exit_handlers_ran = 0;

struct Farewell {
    ~Farewell() {
        std::printf("static destructor ran after %d exit handlers and %d thread_local destructors\n",
                    exit_handlers_ran, thread_locals_destroyed.load());
    }
} farewell;

struct PerThread {
    int id = 0;
    ~PerThread() { thread_locals_destroyed.fetch_add(1); }
};

thread_local PerThread per_thread;

struct Custom : std::runtime_error {
    using std::runtime_error::runtime_error;
};

int thrower(int depth) {
    std::vector<std::string> held{"destroyed", "while", "unwinding"};
    if (depth == 0)
        throw Custom("thrown from depth 0");
    return thrower(depth - 1) + static_cast<int>(held.size());
}

// A thread whose handle reaches a third thread before its creator's
// `pthread_create` has returned, and the third thread's join of it.
std::mutex hand_m;
std::condition_variable hand_cv;
bool handed = false;
bool released = false;
pthread_t handed_over;

void* handed_main(void*) {
    std::unique_lock<std::mutex> lock(hand_m);
    handed_over = pthread_self();
    handed = true;
    hand_cv.notify_all();
    hand_cv.wait(lock, [] { return released; });
    return reinterpret_cast<void*>(42);
}

void* joiner_main(void* out) {
    pthread_t handle;
    {
        std::unique_lock<std::mutex> lock(hand_m);
        hand_cv.wait(lock, [] { return handed; });
        handle = handed_over;
    }
    void* got = nullptr;
    int rc = pthread_join(handle, &got);
    *static_cast<intptr_t*>(out) = rc == 0 ? reinterpret_cast<intptr_t>(got) : -rc;
    return nullptr;
}

void* exits_with_42(void*) {
    pthread_exit(reinterpret_cast<void*>(42));
}

void* returns_null(void*) {
    return nullptr;
}

std::mutex detached_m;
std::condition_variable detached_cv;
int detached_ran = 0;

void* detached_main(void*) {
    std::lock_guard<std::mutex> lock(detached_m);
    detached_ran++;
    detached_cv.notify_one();
    return nullptr;
}

template <int N>
struct Counted {
    ~Counted() { exit_handlers_ran++; }
};

template <int N>
void register_one() {
    static Counted<N> counted;
}

template <int... N>
void register_all(std::integer_sequence<int, N...>) {
    (register_one<N>(), ...);
}

const char* errno_name(int e) {
    switch (e) {
    case 0:
        return "0";
    case EPERM:
        return "EPERM";
    case EINVAL:
        return "EINVAL";
    case EDEADLK:
        return "EDEADLK";
    case EAGAIN:
        return "EAGAIN";
    case ETIMEDOUT:
        return "ETIMEDOUT";
    default:
        return "another error";
    }
}

} // namespace

int main() {
    std::vector<int> v(10);
    std::iota(v.begin(), v.end(), 1);
    int sum = std::accumulate(v.begin(), v.end(), 0);
    std::string s = "hello";
    s += ", ToyOS";
    std::cout << "vector sum " << sum << ", back " << v.back() << ", string " << s << " (" << s.size() << ")\n";

    try {
        thrower(5);
    } catch (const std::runtime_error& e) {
        std::cout << "caught " << e.what() << "\n";
    }
    try {
        try {
            throw 42;
        } catch (int n) {
            std::cout << "caught int " << n << ", rethrowing\n";
            throw;
        }
    } catch (int n) {
        std::cout << "caught rethrown int " << n << "\n";
    }
    try {
        (void)std::vector<int>().at(3);
    } catch (const std::out_of_range&) {
        std::cout << "caught out_of_range from at\n";
    }
    try {
        (void)std::stoi("not a number");
    } catch (const std::invalid_argument&) {
        std::cout << "caught invalid_argument from stoi\n";
    }
    try {
        (void)std::stoi("99999999999");
    } catch (const std::out_of_range&) {
        std::cout << "caught out_of_range from stoi\n";
    }

    std::vector<long> partial(4);
    std::vector<std::thread> workers;
    for (int t = 0; t < 4; t++) {
        workers.emplace_back([t, &partial] {
            per_thread.id = t + 1;
            long acc = 0;
            for (long i = t; i < 1000; i += 4)
                acc += i;
            partial[t] = acc;
        });
    }
    for (auto& w : workers)
        w.join();
    std::cout << "threads summed " << std::accumulate(partial.begin(), partial.end(), 0L) << "\n";
    std::cout << "thread_local destructors ran " << thread_locals_destroyed.load() << "\n";

    std::mutex m;
    std::condition_variable cv;
    int stage = 0;
    std::thread ping([&] {
        std::unique_lock<std::mutex> lock(m);
        cv.wait(lock, [&] { return stage == 1; });
        stage = 2;
        cv.notify_one();
    });
    {
        std::lock_guard<std::mutex> lock(m);
        stage = 1;
    }
    cv.notify_one();
    {
        std::unique_lock<std::mutex> lock(m);
        cv.wait(lock, [&] { return stage == 2; });
    }
    ping.join();
    std::cout << "condition variable handshake done\n";

    std::exception_ptr carried;
    std::thread failing([&] {
        try {
            throw std::logic_error("from a thread");
        } catch (...) {
            carried = std::current_exception();
        }
    });
    failing.join();
    try {
        std::rethrow_exception(carried);
    } catch (const std::logic_error& e) {
        std::cout << "rethrew " << e.what() << "\n";
    }

    pthread_key_t key;
    pthread_key_create(&key, nullptr);
    pthread_setspecific(key, &key);
    void* seen = &seen;
    std::thread::id other;
    std::thread reader([&] {
        seen = pthread_getspecific(key);
        other = std::this_thread::get_id();
    });
    reader.join();
    std::cout << "a key set in main reads " << (seen == nullptr ? "null" : "set") << " in another thread and "
              << (pthread_getspecific(key) == &key ? "set" : "lost") << " in main; thread ids "
              << (other != std::this_thread::get_id() ? "differ" : "match") << "\n";

    intptr_t joined = 0;
    pthread_t joiner, handed_thread;
    pthread_create(&joiner, nullptr, joiner_main, &joined);
    pthread_create(&handed_thread, nullptr, handed_main, nullptr);
    {
        std::lock_guard<std::mutex> lock(hand_m);
        released = true;
    }
    hand_cv.notify_all();
    pthread_join(joiner, nullptr);
    std::cout << "a thread joined by a third before its creator returned gave " << joined << "\n";

    pthread_t exiting;
    void* exited = nullptr;
    pthread_create(&exiting, nullptr, exits_with_42, nullptr);
    pthread_join(exiting, &exited);
    std::cout << "pthread_exit handed its join " << reinterpret_cast<intptr_t>(exited) << "\n";

    // 64 stacks of 64 MiB each way, more than the guest's memory
    // (`tests/common/qemu.rs`'s `-m`): each is freed, or a create runs out.
    constexpr int stacks = 64;
    pthread_attr_t big;
    pthread_attr_init(&big);
    pthread_attr_setstacksize(&big, size_t{64} << 20);
    int joined_stacks = 0;
    int detached_stacks = 0;
    int refused = 0;
    while (joined_stacks < stacks && refused == 0) {
        pthread_t t;
        refused = pthread_create(&t, &big, returns_null, nullptr);
        if (refused == 0) {
            pthread_join(t, nullptr);
            joined_stacks++;
        }
    }
    pthread_attr_setdetachstate(&big, PTHREAD_CREATE_DETACHED);
    while (detached_stacks < stacks && refused == 0) {
        pthread_t t;
        refused = pthread_create(&t, &big, detached_main, nullptr);
        if (refused == 0)
            detached_stacks++;
    }
    {
        std::unique_lock<std::mutex> lock(detached_m);
        detached_cv.wait(lock, [&] { return detached_ran == detached_stacks; });
    }
    std::cout << "threads with 64 MiB stacks: " << joined_stacks << " joined, " << detached_stacks
              << " detached, the last create answering " << errno_name(refused) << "\n";

    std::recursive_mutex recursive;
    recursive.lock();
    bool again = recursive.try_lock();
    if (again)
        recursive.unlock();
    recursive.unlock();
    pthread_mutexattr_t checking;
    pthread_mutexattr_init(&checking);
    pthread_mutexattr_settype(&checking, PTHREAD_MUTEX_ERRORCHECK);
    pthread_mutex_t checked;
    pthread_mutex_init(&checked, &checking);
    pthread_mutex_lock(&checked);
    int relock = pthread_mutex_lock(&checked);
    pthread_mutex_unlock(&checked);
    std::cout << "a recursive mutex takes a second lock: " << (again ? "yes" : "no")
              << "; an error-checking one answers " << errno_name(relock) << "\n";

    char small[4];
    int whole = std::snprintf(small, sizeof small, "%d", 123456);
    std::cout << "snprintf into 4 bytes answers " << whole << " and holds " << small << "\n";
    int printed = std::printf("%04100d\n", 42);
    std::cout << "printf printed " << printed << " bytes\n";
    pthread_attr_t huge;
    pthread_attr_init(&huge);
    int no_stack = pthread_attr_setstacksize(&huge, SIZE_MAX);
    std::cout << "a stack of SIZE_MAX bytes: " << errno_name(no_stack) << "; getentropy of nothing: "
              << getentropy(nullptr, 0) << "\n";

    std::ostringstream out;
    out << std::stod("2.5") * 4 << ' ' << std::to_string(-17) << ' ' << std::stoull("18446744073709551615");
    std::cout << "stream " << out.str() << "\n";
    std::wstring wide = L"wide " + std::to_wstring(123);
    std::cout << "wide length " << wide.size() << ", last " << static_cast<char>(wide.back()) << "\n";
    std::map<std::string, int> counts;
    for (const char* word : {"a", "b", "a"})
        counts[word]++;
    std::cout << "map a=" << counts["a"] << " b=" << counts["b"] << "\n";

    {
        std::mutex tm;
        std::condition_variable tcv;
        std::unique_lock<std::mutex> lock(tm);
        bool woke = tcv.wait_for(lock, std::chrono::milliseconds(10), [] { return false; });
        bool ready = false;
        std::thread notifier([&] {
            std::lock_guard<std::mutex> g(tm);
            ready = true;
            tcv.notify_one();
        });
        auto notified = std::cv_status::no_timeout;
        while (!ready && notified == std::cv_status::no_timeout)
            notified = tcv.wait_until(lock, std::chrono::steady_clock::now() + std::chrono::seconds(30));
        lock.unlock();
        notifier.join();
        std::cout << "a wait nobody ends " << (woke ? "was woken" : "timed out") << ", a notified one answered "
                  << (notified == std::cv_status::no_timeout ? "no_timeout" : "timeout") << "\n";
    }

    std::cout << "a condition wait on a mutex it does not hold answers";
    for (int type : {PTHREAD_MUTEX_ERRORCHECK, PTHREAD_MUTEX_RECURSIVE}) {
        pthread_mutexattr_t attr;
        pthread_mutexattr_init(&attr);
        pthread_mutexattr_settype(&attr, type);
        pthread_mutex_t unheld;
        pthread_mutex_init(&unheld, &attr);
        pthread_cond_t cond;
        pthread_cond_init(&cond, nullptr);
        timespec at;
        clock_gettime(CLOCK_REALTIME, &at);
        at.tv_sec += 1;
        int timed = pthread_cond_timedwait(&cond, &unheld, &at);
        int plain = pthread_cond_wait(&cond, &unheld);
        std::cout << (type == PTHREAD_MUTEX_ERRORCHECK ? " error-checking " : ", recursive ") << errno_name(timed)
                  << " and " << errno_name(plain);
    }
    std::cout << "\n";

    register_all(std::make_integer_sequence<int, 40>{});
    per_thread.id = 0;
    std::cout << "done\n";
    return 0;
}
