#include "llama.h"
#include "nlohmann/json.hpp"
#include <algorithm>
#include <chrono>
#include <clocale>
#include <cstdlib>
#include <iostream>
#include <memory>
#include <stdexcept>
#include <string>
#include <thread>
#include <vector>
#ifdef _WIN32
#include <windows.h>
#else
#include <unistd.h>
#endif

using json = nlohmann::json;

static void watch_parent(uint32_t pid) {
#ifdef _WIN32
    HANDLE parent = OpenProcess(SYNCHRONIZE, FALSE, pid);
    if (!parent) {
        std::_Exit(1);
    }
    std::thread([parent]() {
        WaitForSingleObject(parent, INFINITE);
        CloseHandle(parent);
        std::_Exit(1);
    }).detach();
#else
    if (pid != static_cast<uint32_t>(getppid())) {
        std::_Exit(1);
    }
    std::thread([pid]() {
        while (pid == static_cast<uint32_t>(getppid())) {
            std::this_thread::sleep_for(std::chrono::milliseconds(250));
        }
        std::_Exit(1);
    }).detach();
#endif
}

static void set_batch(llama_batch_ext *batch, const llama_token *tokens, int32_t count, llama_pos start) {
    llama_batch_ext_clear(batch);
    for (int32_t index = 0; index < count; ++index) {
        int32_t entry = llama_batch_ext_add_token(batch, 0, tokens[index]);
        llama_pos position = start + index;
        if (entry < 0 || !llama_batch_ext_set_pos(batch, entry, &position)) {
            throw std::runtime_error("batch failed");
        }
    }
    if (!llama_batch_ext_set_output_logits(batch, count - 1, true)) {
        throw std::runtime_error("logits failed");
    }
}

static json generate(const std::string &path, const json &request) {
    const std::string system = request.at("system").get<std::string>();
    const std::string user = request.at("user").get<std::string>();
    const int32_t maximum = request.at("max_tokens").get<int32_t>();
    if (maximum < 1 || maximum > 2048 || system.size() + user.size() > 24 * 1024
        || system.find("<|im_") != std::string::npos || user.find("<|im_") != std::string::npos) {
        throw std::runtime_error("input outside limits");
    }
    watch_parent(request.at("parent_pid").get<uint32_t>());
    llama_log_set([](ggml_log_level, const char *, void *) {}, nullptr);
    llama_backend_init();
    auto parameters = llama_model_default_params();
    parameters.n_gpu_layers = 0;
    std::unique_ptr<llama_model, decltype(&llama_model_free)> model(
        llama_model_load_from_file(path.c_str(), parameters), llama_model_free);
    if (!model) {
        throw std::runtime_error("model unavailable");
    }
    const auto *vocab = llama_model_get_vocab(model.get());
    const std::string prompt = "<|im_start|>system\n" + system
        + "<|im_end|>\n<|im_start|>user\n" + user + "<|im_end|>\n<|im_start|>assistant\n";
    const int32_t prompt_bytes = static_cast<int32_t>(prompt.size());
    const int32_t count = -llama_tokenize(vocab, prompt.data(), prompt_bytes, nullptr, 0, true, true);
    if (count < 1 || count + maximum > 8192) {
        throw std::runtime_error("context limit exceeded");
    }
    std::vector<llama_token> tokens(count);
    if (llama_tokenize(vocab, prompt.data(), prompt_bytes, tokens.data(), count, true, true) != count) {
        throw std::runtime_error("tokenization failed");
    }
    auto context_parameters = llama_context_default_params();
    context_parameters.n_ctx = count + maximum;
    context_parameters.n_batch = 512;
    context_parameters.n_ubatch = 512;
    context_parameters.n_threads = static_cast<int32_t>(std::min(4u, std::max(1u, std::thread::hardware_concurrency())));
    context_parameters.n_threads_batch = context_parameters.n_threads;
    context_parameters.no_perf = true;
    std::unique_ptr<llama_context, decltype(&llama_free)> context(
        llama_init_from_model(model.get(), context_parameters), llama_free);
    if (!context) {
        throw std::runtime_error("context unavailable");
    }
    std::unique_ptr<llama_batch_ext, decltype(&llama_batch_ext_free)> batch(
        llama_batch_ext_init(context.get()), llama_batch_ext_free);
    if (!batch) {
        throw std::runtime_error("batch unavailable");
    }
    for (int32_t offset = 0; offset < count; offset += 512) {
        const int32_t size = std::min(512, count - offset);
        set_batch(batch.get(), tokens.data() + offset, size, offset);
        if (llama_process(context.get(), LLAMA_PROCESS_TYPE_DECODE, batch.get()) != 0) {
            throw std::runtime_error("prompt processing failed");
        }
    }
    std::unique_ptr<llama_sampler, decltype(&llama_sampler_free)> sampler(
        llama_sampler_init_greedy(), llama_sampler_free);
    std::string text;
    text.reserve(4096);
    int32_t generated = 0;
    std::string finish_reason = "length";
    for (; generated < maximum; ++generated) {
        const llama_token token = llama_sampler_sample(sampler.get(), context.get(), -1);
        if (llama_vocab_is_eog(vocab, token)) {
            finish_reason = "stop";
            break;
        }
        char buffer[256];
        const int32_t bytes = llama_token_to_piece(vocab, token, buffer, sizeof(buffer), 0, false);
        if (bytes < 0 || text.size() + static_cast<size_t>(bytes) > 64 * 1024) {
            throw std::runtime_error("output outside limits");
        }
        text.append(buffer, static_cast<size_t>(bytes));
        if (generated + 1 < maximum) {
            set_batch(batch.get(), &token, 1, count + generated);
            if (llama_process(context.get(), LLAMA_PROCESS_TYPE_DECODE, batch.get()) != 0) {
                throw std::runtime_error("generation failed");
            }
        }
    }
    return json{{"text", text}, {"finish_reason", finish_reason},
                {"prompt_tokens", count}, {"completion_tokens", generated}};
}

int main(int argc, char **argv) {
    std::setlocale(LC_NUMERIC, "C");
    if (argc == 2 && std::string(argv[1]) == "--version") {
        std::cout << "sona-cleanup 1 llama.cpp 620d2225cc436ab0b001e3200e590e3b286ef9ab\n";
        return 0;
    }
    if (argc != 2) {
        return 2;
    }
    try {
        std::string input;
        input.reserve(32 * 1024);
        char character;
        while (std::cin.get(character)) {
            if (input.size() >= 128 * 1024) {
                return 2;
            }
            input.push_back(character);
        }
        const json result = generate(argv[1], json::parse(input));
        std::cout << result.dump() << '\n';
        return 0;
    } catch (const std::exception &) {
        std::cerr << "offline cleanup could not complete\n";
        return 1;
    }
}
