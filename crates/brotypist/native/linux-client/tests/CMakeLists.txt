cmake_minimum_required(VERSION 3.21)
project(BroTypistOwnedClients LANGUAGES CXX)
if(NOT CMAKE_SYSTEM_NAME STREQUAL "Linux")
  message(FATAL_ERROR "Owned client acceptance requires Linux and a private virtual desktop")
endif()
find_package(PkgConfig REQUIRED)
pkg_check_modules(GTK3 REQUIRED IMPORTED_TARGET gtk+-3.0)
find_package(Qt6 6.12.0 EXACT REQUIRED COMPONENTS Core Gui Widgets)
# Deliberately do not compile the adapters into these hosts. Their real input
# contexts must come from the reviewed, privately staged patched Fcitx plugins.
# GTK: ../gtk/brotypist-source.cmake (5.1.7, static patched GClient).
# Qt: ../qt/BUILDING.txt (5.1.16, exact Qt6.12.0 private ABI).
add_executable(brotypist-gtk-fixture gtk_fixture.cpp)
target_link_libraries(brotypist-gtk-fixture PRIVATE PkgConfig::GTK3)
target_compile_features(brotypist-gtk-fixture PRIVATE cxx_std_17)
add_executable(brotypist-qt-fixture qt_fixture.cpp)
target_link_libraries(brotypist-qt-fixture PRIVATE Qt6::Core Qt6::Gui Qt6::Widgets)
target_compile_features(brotypist-qt-fixture PRIVATE cxx_std_17)
