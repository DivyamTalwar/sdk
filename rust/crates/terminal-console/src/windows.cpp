#include "acyclic-terminal-console/src/lib.rs.h"
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <stdexcept>
#include <string>

namespace acyclic_terminal {
static void failure(DWORD error) { throw std::runtime_error("Windows console error " + std::to_string(error)); }
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
rust::Vec<std::uint8_t> console_read(std::size_t input, std::uint32_t timeout) {
    const auto handle = reinterpret_cast<HANDLE>(input);
    const auto deadline = GetTickCount64() + timeout;
    rust::Vec<std::uint8_t> result;
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
            if (!ReadFile(handle, bytes, sizeof(bytes), &count, nullptr)) failure(GetLastError());
            for (DWORD i = 0; i < count; ++i) result.push_back(static_cast<std::uint8_t>(bytes[i]));
            return result;
        }
        if (!ReadConsoleInputW(handle, &event, 1, &count)) failure(GetLastError());
    }
    return result;
}
}
