#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "esp_log.h"

extern void esp_agent_configure(unsigned int interval_ms);

static const char *TAG = "app";

void app_main(void)
{
    // Optional: overrides the default 1000 ms sampling interval.
    esp_agent_configure(2000);

    ESP_LOGI(TAG, "esp_agent running - connect esp-tui to see telemetry");

    while (1) {
        vTaskDelay(pdMS_TO_TICKS(10000));
    }
}
