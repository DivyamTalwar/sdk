#pragma once
#include "rust/cxx.h"
#include <cstddef>
#include <cstdint>
namespace acyclic_terminal {
struct ConsoleState;
ConsoleState console_state(std::size_t input, std::size_t output);
void console_apply(std::size_t input, std::size_t output, ConsoleState state);
void console_register();
void console_break();
rust::Vec<std::uint8_t> console_read(std::size_t input, std::size_t capacity, std::uint32_t timeout);
}
