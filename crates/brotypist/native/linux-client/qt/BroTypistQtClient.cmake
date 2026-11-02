# SPDX-License-Identifier: BSD-3-Clause
# Included only by the reviewed fcitx5-qt 5.1.16 Qt6 plugin patch.
function(brotypist_attach_qt6_client target)
    if(NOT TARGET "${target}")
        message(FATAL_ERROR "BroTypist requires the existing Fcitx Qt6 plugin target")
    endif()
    if(NOT Qt6Core_VERSION VERSION_EQUAL "6.12.0")
        message(FATAL_ERROR "Private CachyOS trial requires the exact Qt 6.12.0 SDK/private ABI")
    endif()
    target_sources("${target}" PRIVATE
        "${CMAKE_CURRENT_FUNCTION_LIST_DIR}/brotypistqtclient.cpp"
        "${CMAKE_CURRENT_FUNCTION_LIST_DIR}/brotypistqtclient.h")
    target_include_directories("${target}" PRIVATE "${CMAKE_CURRENT_FUNCTION_LIST_DIR}")
    target_compile_definitions("${target}" PRIVATE BROTYPIST_NATIVE_QT6=1)
    set_target_properties("${target}" PROPERTIES AUTOMOC TRUE)
endfunction()
