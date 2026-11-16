// esp-idf-sys'in varsayılan bağlantılarında olmayabilecek başlıklar
#include "esp_lcd_panel_rgb.h"
#include "esp_lcd_panel_ops.h"
#include "esp_psram.h"
#include "esp_heap_caps.h"

// USB host: QMX'in ses (I/Q) ve seri (CAT) arayüzleri
#include "usb/usb_host.h"
#include "usb/usb_helpers.h"
#include "usb/uac_host.h"
#include "usb/cdc_acm_host.h"
