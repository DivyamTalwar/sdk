#pragma once
#include "rust/cxx.h"
#include <cstddef>
#include <cstdint>
#include <memory>
namespace acyclic_terminal {
struct ConsoleState;
struct ConsoleWrite;
struct ConsoleWriteResult;
ConsoleState console_state(std::size_t input, std::size_t output);
void console_apply(std::size_t input, std::size_t output, ConsoleState state);
void console_register();
void console_break();
rust::Vec<std::uint8_t> console_read(std::size_t input, std::size_t capacity, std::uint32_t timeout);
std::shared_ptr<ConsoleWrite> console_write_start(std::size_t output, rust::Slice<const std::uint8_t> bytes);
ConsoleWriteResult console_write_wait(const std::shared_ptr<ConsoleWrite>& write, std::uint32_t timeout);
void console_write_cancel(const std::shared_ptr<ConsoleWrite>& write);
void console_cancel(std::size_t output);
}
