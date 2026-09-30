// What `cxx_runtime` compiles with the toolchain's clang and runs on ToyOS:
// libc++'s containers, strings and streams, exceptions through frames with
// destructors, threads with their thread_local and static destructors, and
// libc's per-thread keys. Every line it prints is the same on any host.
#include <atomic>
#include <condition_variable>
#include <cstdio>
#include <exception>
#include <iostream>
#include <map>
#include <mutex>
#include <numeric>
#include <pthread.h>
#include <sstream>
#include <stdexcept>
#include <string>
#include <thread>
#include <vector>

namespace {

struct Farewell {
    ~Farewell() { std::printf("static destructor ran\n"); }
} farewell;

std::atomic<int> thread_locals_destroyed{0};

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

    std::ostringstream out;
    out << std::stod("2.5") * 4 << ' ' << std::to_string(-17) << ' ' << std::stoull("18446744073709551615");
    std::cout << "stream " << out.str() << "\n";
    std::wstring wide = L"wide " + std::to_wstring(123);
    std::cout << "wide length " << wide.size() << ", last " << static_cast<char>(wide.back()) << "\n";
    std::map<std::string, int> counts;
    for (const char* word : {"a", "b", "a"})
        counts[word]++;
    std::cout << "map a=" << counts["a"] << " b=" << counts["b"] << "\n";
    std::cout << "done\n";
    return 0;
}
