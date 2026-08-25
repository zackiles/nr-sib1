#ifndef NR_SIB1_H
#define NR_SIB1_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

enum nr_sib1_status {
    NR_SIB1_OK = 0,
    NR_SIB1_INVALID_ARGUMENT = 1,
    NR_SIB1_INVALID_UTF8 = 2,
    NR_SIB1_INVALID_CONFIG = 3,
    NR_SIB1_SERIALIZATION_FAILED = 4,
    NR_SIB1_PANICKED = 5
};

int32_t nr_sib1_decode(const float *iq,
                       size_t iq_len,
                       const uint8_t *config,
                       size_t config_len,
                       uint8_t **output,
                       size_t *output_len);

void nr_sib1_free(uint8_t *data, size_t len);

#ifdef __cplusplus
}
#endif

#endif
