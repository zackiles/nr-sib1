#ifndef GR_NR_SIB1_DECODER_HPP
#define GR_NR_SIB1_DECODER_HPP

#include <gnuradio-4.0/Block.hpp>
#include <gnuradio-4.0/BlockRegistry.hpp>

#include <complex>
#include <cstdint>
#include <stdexcept>
#include <string>
#include <vector>

#include "nr_sib1.h"

namespace gr::nr_sib1 {

GR_REGISTER_BLOCK(gr::nr_sib1::Decoder)
struct Decoder : Block<Decoder> {
    PortIn<std::vector<std::complex<float>>> in;
    PortOut<std::string> events;

    std::string config_json;

    GR_MAKE_REFLECTABLE(Decoder, in, events, config_json);

    [[nodiscard]] std::string processOne(const std::vector<std::complex<float>>& window) const {
        std::vector<float> iq;
        iq.reserve(window.size() * 2UZ);
        for (const auto& sample : window) {
            iq.push_back(sample.real());
            iq.push_back(sample.imag());
        }

        std::uint8_t* output = nullptr;
        std::size_t output_len = 0;
        const auto status = nr_sib1_decode(
            iq.data(),
            iq.size(),
            reinterpret_cast<const std::uint8_t*>(config_json.data()),
            config_json.size(),
            &output,
            &output_len);
        if (status != NR_SIB1_OK) {
            throw std::runtime_error("nr-sib1 decode failed with status " + std::to_string(status));
        }
        std::string json(reinterpret_cast<const char*>(output), output_len);
        nr_sib1_free(output, output_len);
        return json;
    }
};

} // namespace gr::nr_sib1

#endif
