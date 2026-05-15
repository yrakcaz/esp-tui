if("${IDF_TARGET}" STREQUAL "esp32")
    set(_AGENT_TRIPLE "xtensa-esp32-espidf")
elseif("${IDF_TARGET}" STREQUAL "esp32s2")
    set(_AGENT_TRIPLE "xtensa-esp32s2-espidf")
elseif("${IDF_TARGET}" STREQUAL "esp32s3")
    set(_AGENT_TRIPLE "xtensa-esp32s3-espidf")
elseif("${IDF_TARGET}" STREQUAL "esp32c3" OR "${IDF_TARGET}" STREQUAL "esp32c2")
    set(_AGENT_TRIPLE "riscv32imc-esp-espidf")
elseif("${IDF_TARGET}" STREQUAL "esp32c6" OR "${IDF_TARGET}" STREQUAL "esp32h2")
    set(_AGENT_TRIPLE "riscv32imac-esp-espidf")
else()
    message(FATAL_ERROR "Unsupported IDF_TARGET: ${IDF_TARGET}")
endif()

set(_AGENT_LIB
    "${CMAKE_CURRENT_LIST_DIR}/../../../target/${_AGENT_TRIPLE}/release/libesp_agent.a"
)

idf_component_register(
    SRCS "main.c"
    INCLUDE_DIRS "."
    REQUIRES nvs_flash esp_wifi esp_hw_support
)

target_link_options(${COMPONENT_LIB} INTERFACE
    -Wl,--undefined=_esp_agent_ctor
    -Wl,--undefined=esp_chip_info
    -Wl,--undefined=esp_read_mac
    -Wl,--undefined=esp_wifi_sta_get_ap_info
    -Wl,--undefined=nvs_get_stats
)
target_link_libraries(${COMPONENT_LIB} INTERFACE ${_AGENT_LIB})
