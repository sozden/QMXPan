// QMXPan için ft8_lib sarmalayıcısı. Çözme akışı ve çağrı işareti karma
// tablosu ft8_lib'in demo/decode_ft8.c dosyasından uyarlanmıştır
// (MIT Lisansı, Copyright (c) 2018 Kārlis Goba).
#include "ft8w.h"

#include <string.h>
#include <stdio.h>

#include <math.h>

#include <ft8/decode.h>
#include <ft8/encode.h>
#include <ft8/message.h>
#include <common/monitor.h>

#define MAX_CANDIDATES_CAP 200
#define MAX_DECODED 60

static int s_min_score = 10;
static int s_max_cands = 120;
static int s_ldpc_iters = 20;

void ft8w_set_params(int min_score, int max_candidates, int ldpc_iterations)
{
    s_min_score = min_score;
    s_max_cands = max_candidates > MAX_CANDIDATES_CAP ? MAX_CANDIDATES_CAP : max_candidates;
    s_ldpc_iters = ldpc_iterations;
}

// Şelalenin ortanca değeri (dB): gürültü tabanı
static float waterfall_median_db(const ftx_waterfall_t* wf)
{
    static uint32_t hist[256];
    memset(hist, 0, sizeof(hist));
    size_t n = (size_t)wf->num_blocks * wf->block_stride;
    for (size_t i = 0; i < n; ++i) hist[wf->mag[i]]++;
    size_t half = n / 2, acc = 0;
    for (int v = 0; v < 256; ++v) {
        acc += hist[v];
        if (acc >= half) return (v - 240) * 0.5f;
    }
    return -120.0f;
}

// Çözülen mesajı yeniden kodlayıp 79 sembolün ton gücünü şelaleden okur;
// ortalama gücün gürültü tabanına oranını (bin başına, dB) döndürür.
static float message_power_db(const ftx_waterfall_t* wf, const ftx_candidate_t* cand, const ftx_message_t* msg, float noise_db)
{
    uint8_t tones[FT8_NN];
    ft8_encode(msg->payload, tones);
    float sum = 0.0f;
    int n = 0;
    for (int i = 0; i < FT8_NN; ++i) {
        int block = cand->time_offset + i;
        int bin = cand->freq_offset + tones[i];
        if (block < 0 || block >= wf->num_blocks || bin < 0 || bin >= wf->num_bins) continue;
        int off = block * wf->block_stride + cand->time_sub * wf->freq_osr * wf->num_bins
                + cand->freq_sub * wf->num_bins + bin;
        float db = (wf->mag[off] - 240) * 0.5f;
        sum += powf(10.0f, db / 10.0f);
        n++;
    }
    if (n == 0) return 0.0f;
    return 10.0f * log10f(sum / n) - noise_db;
}

static monitor_t s_mon;
static bool s_ready = false;

// ---------------------------------------------------------------- çağrı karmaları

#define HASH_SIZE 256
static struct {
    char callsign[12];
    uint32_t hash; // üst 8 bit yaş, alt 22 bit karma
} s_hash[HASH_SIZE];

static void hash_cleanup(uint8_t max_age)
{
    for (int i = 0; i < HASH_SIZE; ++i) {
        if (s_hash[i].callsign[0] != '\0') {
            uint8_t age = (uint8_t)(s_hash[i].hash >> 24);
            if (age > max_age) {
                s_hash[i].callsign[0] = '\0';
                s_hash[i].hash = 0;
            } else {
                s_hash[i].hash = (((uint32_t)age + 1u) << 24) | (s_hash[i].hash & 0x3FFFFFu);
            }
        }
    }
}

static void hash_add(const char* callsign, uint32_t hash)
{
    uint16_t hash10 = (hash >> 12) & 0x3FFu;
    int idx = (hash10 * 23) % HASH_SIZE;
    for (int n = 0; n < HASH_SIZE && s_hash[idx].callsign[0] != '\0'; ++n) {
        if (((s_hash[idx].hash & 0x3FFFFFu) == hash) && strcmp(s_hash[idx].callsign, callsign) == 0) {
            s_hash[idx].hash &= 0x3FFFFFu; // yaşı sıfırla
            return;
        }
        idx = (idx + 1) % HASH_SIZE;
    }
    if (s_hash[idx].callsign[0] != '\0') return; // tablo dolu
    strncpy(s_hash[idx].callsign, callsign, 11);
    s_hash[idx].callsign[11] = '\0';
    s_hash[idx].hash = hash;
}

static bool hash_lookup(ftx_callsign_hash_type_t type, uint32_t hash, char* callsign)
{
    uint8_t shift = (type == FTX_CALLSIGN_HASH_10_BITS) ? 12 : (type == FTX_CALLSIGN_HASH_12_BITS ? 10 : 0);
    uint16_t hash10 = (hash >> (12 - shift)) & 0x3FFu;
    int idx = (hash10 * 23) % HASH_SIZE;
    for (int n = 0; n < HASH_SIZE && s_hash[idx].callsign[0] != '\0'; ++n) {
        if (((s_hash[idx].hash & 0x3FFFFFu) >> shift) == hash) {
            strcpy(callsign, s_hash[idx].callsign);
            return true;
        }
        idx = (idx + 1) % HASH_SIZE;
    }
    callsign[0] = '\0';
    return false;
}

static ftx_callsign_hash_interface_t s_hash_if = {
    .lookup_hash = hash_lookup,
    .save_hash = hash_add,
};

// ---------------------------------------------------------------------- API

int ft8w_init(float f_min, float f_max)
{
    if (s_ready) return 0;
    monitor_config_t cfg = {
        .f_min = f_min,
        .f_max = f_max,
        .sample_rate = 12000,
        .time_osr = 2,
        .freq_osr = 2,
        .protocol = FTX_PROTOCOL_FT8,
    };
    memset(s_hash, 0, sizeof(s_hash));
    monitor_init(&s_mon, &cfg);
    if (s_mon.wf.mag == NULL || s_mon.window == NULL || s_mon.fft_work == NULL) return -1;
    s_ready = true;
    return 0;
}

int ft8w_block_size(void) { return s_ready ? s_mon.block_size : 0; }

void ft8w_reset(void)
{
    if (s_ready) monitor_reset(&s_mon);
}

void ft8w_process(const float* block)
{
    if (s_ready) monitor_process(&s_mon, block);
}

int ft8w_num_blocks(void) { return s_ready ? s_mon.wf.num_blocks : 0; }

// Büyük diziler yığında değil (görev yığını sınırlı)
static ftx_candidate_t s_cands[MAX_CANDIDATES_CAP];
static ftx_message_t s_decoded[MAX_DECODED];
static ftx_message_t* s_decoded_ht[MAX_DECODED];

int ft8w_decode(ft8w_result_t* out, int max)
{
    if (!s_ready) return 0;
    const ftx_waterfall_t* wf = &s_mon.wf;
    float noise_db = waterfall_median_db(wf);
    int num_cands = ftx_find_candidates(wf, s_max_cands, s_cands, s_min_score);
    for (int i = 0; i < MAX_DECODED; ++i) s_decoded_ht[i] = NULL;

    int n_out = 0;
    for (int idx = 0; idx < num_cands && n_out < max; ++idx) {
        const ftx_candidate_t* cand = &s_cands[idx];
        ftx_message_t msg;
        ftx_decode_status_t status;
        if (!ftx_decode_candidate(wf, cand, s_ldpc_iters, &msg, &status)) continue;

        // Aynı mesaj birden fazla adayda çözülmüş olabilir
        int h = msg.hash % MAX_DECODED;
        bool empty = false, dup = false;
        for (int n = 0; n < MAX_DECODED; ++n) {
            if (s_decoded_ht[h] == NULL) { empty = true; break; }
            if (s_decoded_ht[h]->hash == msg.hash && memcmp(s_decoded_ht[h]->payload, msg.payload, sizeof(msg.payload)) == 0) {
                dup = true;
                break;
            }
            h = (h + 1) % MAX_DECODED;
        }
        if (dup || !empty) continue;
        memcpy(&s_decoded[h], &msg, sizeof(msg));
        s_decoded_ht[h] = &s_decoded[h];

        ft8w_result_t* r = &out[n_out++];
        ftx_message_offsets_t offsets;
        if (ftx_message_decode(&msg, &s_hash_if, r->text, &offsets) != FTX_MESSAGE_RC_OK) {
            snprintf(r->text, sizeof(r->text), "(unpack error)");
        }
        r->freq_hz = (s_mon.min_bin + cand->freq_offset + (float)cand->freq_sub / wf->freq_osr) / s_mon.symbol_period;
        r->time_sec = (cand->time_offset + (float)cand->time_sub / wf->time_osr) * s_mon.symbol_period;
        // ft8_lib'in demosundaki gibi kaba tahmin (WSJT-X SNR'si değil)
        r->snr = (int16_t)(cand->score * 0.5f - 22.0f);
        r->sig_db = message_power_db(wf, cand, &msg, noise_db);
    }
    hash_cleanup(10);
    return n_out;
}
