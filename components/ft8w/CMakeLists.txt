# ft8_lib (MIT) kaynakları + QMXPan sarmalayıcısı. PortAudio/WAV kodu alınmaz.
set(L ${CMAKE_CURRENT_LIST_DIR}/../ft8_lib)
idf_component_register(
    SRCS "ft8w.c"
         "${L}/ft8/constants.c" "${L}/ft8/crc.c" "${L}/ft8/decode.c" "${L}/ft8/encode.c"
         "${L}/ft8/ldpc.c" "${L}/ft8/message.c" "${L}/ft8/text.c"
         "${L}/fft/kiss_fft.c" "${L}/fft/kiss_fftr.c"
         "${L}/common/monitor.c"
    INCLUDE_DIRS "."
    PRIV_INCLUDE_DIRS "${L}"
)
# Kütüphane kendi uyarılarıyla derlensin, hızlı olsun
target_compile_options(${COMPONENT_LIB} PRIVATE -O2 -w)
