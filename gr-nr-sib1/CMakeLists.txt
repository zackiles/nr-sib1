cmake_minimum_required(VERSION 3.27)
project(gr-nr-sib1 VERSION 0.1.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 23)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

find_package(gnuradio4 CONFIG REQUIRED)

set(NR_SIB1_FFI_LIBRARY
    "${CMAKE_CURRENT_LIST_DIR}/../target/release/libnr_sib1_ffi.a"
    CACHE FILEPATH "Path to the nr-sib1-ffi static library")

if(NOT EXISTS "${NR_SIB1_FFI_LIBRARY}")
  message(FATAL_ERROR "Build nr-sib1-ffi first: cargo build --release -p nr-sib1-ffi")
endif()

add_library(gr-nr-sib1 INTERFACE)
target_include_directories(gr-nr-sib1 INTERFACE
  $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
  $<BUILD_INTERFACE:${CMAKE_CURRENT_LIST_DIR}/../crates/nr-sib1-ffi/include>
  $<INSTALL_INTERFACE:include>)
target_link_libraries(gr-nr-sib1 INTERFACE
  gnuradio4::gnuradio-core
  "${NR_SIB1_FFI_LIBRARY}")

install(TARGETS gr-nr-sib1 EXPORT gr-nr-sib1-targets)
install(DIRECTORY include/ DESTINATION include)
