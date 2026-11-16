// QMXPan için ft8_lib (MIT, Kārlis Goba) üstüne ince C sarmalayıcı.
// Rust tarafı yalnızca bu başlığı görür.
#pragma once
#include <stdint.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    float freq_hz;  // ses frekansı (USB'de ayar frekansının üstü)
    float time_sec; // dilim başlangıcına göre zaman kayması
    int16_t snr;    // kaba SNR tahmini (dB)
    char text[36];  // çözülen mesaj
} ft8w_result_t;

// 12 kHz ses için çözücüyü hazırlar (f_min..f_max Hz arası aranır). 0 = başarılı.
int ft8w_init(float f_min, float f_max);
// Bir FT8 sembol bloğundaki örnek sayısı (12 kHz'de 1920)
int ft8w_block_size(void);
// Dilim başında çağrılır (biriken şelaleyi sıfırlar)
void ft8w_reset(void);
// Bir blok (ft8w_block_size() örnek) ses ekler
void ft8w_process(const float* block);
// Biriken blok sayısı
int ft8w_num_blocks(void);
// Biriken veriyi çözer; en fazla max sonuç yazar, sayısını döndürür
int ft8w_decode(ft8w_result_t* out, int max);

#ifdef __cplusplus
}
#endif
