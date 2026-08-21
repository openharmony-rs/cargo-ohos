cmake_minimum_required(VERSION 3.16)
project(fixture C)

add_library(fixture STATIC fixture.c)

# The generated toolchain file has to arrive with a cross-compiler and the OHOS sysroot.
if(NOT CMAKE_C_COMPILER MATCHES "clang")
  message(FATAL_ERROR "expected a clang from the OHOS toolchain, got ${CMAKE_C_COMPILER}")
endif()
if(NOT CMAKE_C_FLAGS MATCHES "--sysroot")
  message(FATAL_ERROR "the OHOS sysroot did not reach cmake: ${CMAKE_C_FLAGS}")
endif()

install(TARGETS fixture ARCHIVE DESTINATION lib)
