#include "acyclic-terminal-console/src/lib.rs.h"
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <algorithm>
#include <stdexcept>
#include <string>
#include <vector>
#include <atomic>

namespace acyclic_terminal {
static void failure(DWORD error) { throw std::runtime_error("Windows console error " + std::to_string(error)); }
struct ConsoleWrite {
    HANDLE output;
    OVERLAPPED operation{};
    std::vector<std::uint8_t> bytes;
    HANDLE thread = nullptr;
    HANDLE completed = nullptr;
    std::atomic<bool> cancelled{false};
    DWORD error = ERROR_SUCCESS;
    DWORD written = 0;
    ~ConsoleWrite() {
        if (operation.hEvent) CloseHandle(operation.hEvent);
        if (thread) CloseHandle(thread);
        if (completed) CloseHandle(completed);
    }
};
static DWORD WINAPI write_worker(void* context) {
    auto write = *static_cast<std::shared_ptr<ConsoleWrite>*>(context);
    delete static_cast<std::shared_ptr<ConsoleWrite>*>(context);
    if (write->cancelled.load()) {
        write->error = ERROR_OPERATION_ABORTED;
    } else if (!WriteFile(write->output, write->bytes.data(), static_cast<DWORD>(write->bytes.size()),
                          &write->written, &write->operation)) {
        write->error = GetLastError();
        if (write->error == ERROR_IO_PENDING) {
            if (write->cancelled.load()) CancelIoEx(write->output, &write->operation);
            write->error = GetOverlappedResult(write->output, &write->operation, &write->written, TRUE)
                ? ERROR_SUCCESS : GetLastError();
        }
    }
    SetEvent(write->completed);
    return 0;
}
std::shared_ptr<ConsoleWrite> console_write_start(std::size_t output, rust::Slice<const std::uint8_t> bytes) {
    auto write = std::make_shared<ConsoleWrite>();
    write->output = reinterpret_cast<HANDLE>(output);
    const auto length = std::min(bytes.size(), std::size_t{4096});
    write->bytes.assign(bytes.begin(), bytes.begin() + length);
    write->operation.hEvent = CreateEventW(nullptr, TRUE, FALSE, nullptr);
    if (!write->operation.hEvent) failure(GetLastError());
    write->completed = CreateEventW(nullptr, TRUE, FALSE, nullptr);
    if (!write->completed) failure(GetLastError());
    auto context = new std::shared_ptr<ConsoleWrite>(write);
    // Console drivers may block even the overlapped submission. The owned
    // worker keeps that driver call outside both the caller and its state lock.
    write->thread = CreateThread(nullptr, 0, write_worker, context, 0, nullptr);
    if (!write->thread) {
        const auto error = GetLastError();
        delete context;
        failure(error);
    }
    return write;
}
ConsoleWriteResult console_write_wait(const std::shared_ptr<ConsoleWrite>& write, std::uint32_t timeout) {
    ConsoleWriteResult result{};
    const auto ready = WaitForSingleObject(write->completed, timeout);
    if (ready != WAIT_OBJECT_0) {
        const auto error = ready == WAIT_TIMEOUT ? ERROR_TIMEOUT : GetLastError();
        // Cancellation is asynchronous. The worker owns its buffer and native
        // operation until completion, without borrowing the Rust owner's data.
        console_write_cancel(write);
        result.error = error;
    } else if (write->error != ERROR_SUCCESS) {
        result.error = write->error;
    } else {
        result.written = write->written;
    }
    return result;
}
void console_write_cancel(const std::shared_ptr<ConsoleWrite>& write) {
    write->cancelled.store(true);
    CancelIoEx(write->output, &write->operation);
    CancelSynchronousIo(write->thread);
}
void console_cancel(std::size_t output) {
    if (!CancelIoEx(reinterpret_cast<HANDLE>(output), nullptr)) {
        const auto error = GetLastError();
        if (error != ERROR_NOT_FOUND) failure(error);
    }
}
ConsoleState console_state(std::size_t input, std::size_t output) {
    ConsoleState state{};
    DWORD input_mode = 0, output_mode = 0;
    if (!GetConsoleMode(reinterpret_cast<HANDLE>(input), &input_mode)) failure(GetLastError());
    if (!GetConsoleMode(reinterpret_cast<HANDLE>(output), &output_mode)) failure(GetLastError());
    state.input_mode = input_mode;
    state.output_mode = output_mode;
    state.input_page = GetConsoleCP();
    state.output_page = GetConsoleOutputCP();
    if (!state.input_page || !state.output_page) failure(GetLastError());
    return state;
}
void console_apply(std::size_t input, std::size_t output, ConsoleState state) {
    DWORD error = 0;
    if (!SetConsoleMode(reinterpret_cast<HANDLE>(input), state.input_mode)) error = GetLastError();
    if (!SetConsoleMode(reinterpret_cast<HANDLE>(output), state.output_mode) && !error) error = GetLastError();
    if (!SetConsoleCP(state.input_page) && !error) error = GetLastError();
    if (!SetConsoleOutputCP(state.output_page) && !error) error = GetLastError();
    if (error) failure(error);
}
static BOOL WINAPI control_handler(DWORD event) {
    // Execute restoration SYNCHRONOUSLY. Queueing a callback then returning is
    // too late for CTRL_CLOSE_EVENT: Windows may kill the process on return.
    return console_control(event) ? TRUE : FALSE;
}
void console_register() {
    if (!SetConsoleCtrlHandler(control_handler, TRUE)) failure(GetLastError());
}
void console_break() {
    if (!GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, 0)) failure(GetLastError());
}
rust::Vec<std::uint8_t> console_read(std::size_t input, std::size_t capacity, std::uint32_t timeout) {
    const auto handle = reinterpret_cast<HANDLE>(input);
    const auto deadline = GetTickCount64() + timeout;
    rust::Vec<std::uint8_t> result;
    if (!capacity) return result;
    for (;;) {
        const auto now = GetTickCount64();
        if (now >= deadline) return result;
        const auto remaining = static_cast<DWORD>(deadline - now);
        const auto ready = WaitForSingleObject(handle, remaining);
        if (ready == WAIT_TIMEOUT) return result;
        if (ready != WAIT_OBJECT_0) failure(GetLastError());
        INPUT_RECORD event{};
        DWORD count = 0;
        if (!PeekConsoleInputW(handle, &event, 1, &count)) failure(GetLastError());
        if (!count) continue;
        const auto key = event.EventType == KEY_EVENT ? event.Event.KeyEvent : KEY_EVENT_RECORD{};
        const auto vk = key.wVirtualKeyCode;
        const bool readable = event.EventType == KEY_EVENT && key.bKeyDown &&
            (key.uChar.UnicodeChar || vk == VK_SPACE || vk == VK_ESCAPE ||
             (vk >= VK_PRIOR && vk <= VK_DOWN) || (vk >= VK_F1 && vk <= VK_F24) ||
             vk == VK_INSERT || vk == VK_DELETE);
        if (readable) {
            char bytes[1024];
            const auto length = static_cast<DWORD>(std::min(capacity, sizeof(bytes)));
            if (!ReadFile(handle, bytes, length, &count, nullptr)) failure(GetLastError());
            for (DWORD i = 0; i < count; ++i) result.push_back(static_cast<std::uint8_t>(bytes[i]));
            return result;
        }
        if (!ReadConsoleInputW(handle, &event, 1, &count)) failure(GetLastError());
    }
    return result;
}
}
