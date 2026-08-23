#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "esp_log.h"

static const char *TAG = "app";

void app_main(void)
{
    ESP_LOGI(TAG, "esp-tui panic demo: connect esp-tui, then wait for the crash");
    vTaskDelay(pdMS_TO_TICKS(5000));

    ESP_LOGI(TAG, "triggering a deliberate panic now");
    // `volatile` stops the compiler from proving this is UB and discarding it;
    // the dereference must actually happen so ESP-IDF's panic handler prints
    // a Guru Meditation Error and Backtrace: dump for esp-tui to decode.
    volatile int *bad_ptr = NULL;
    *bad_ptr = 42;
}
