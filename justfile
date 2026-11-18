# Build, prepare the local transcription model, and install to ~/Applications.
install:
    python3 scripts/install.py --check
    ./build.sh
    python3 scripts/install_model.py
    python3 scripts/install.py

build:
    ./build.sh

# Install an already-built bundle (also used by local verification).
install-built:
    python3 scripts/install.py

verify-install:
    python3 scripts/install.py --verify

# Measure local Vision OCR on generated pages, without opening capture devices.
benchmark-ocr:
    python3 scripts/vendor_speechworks.py --ensure
    RUSTC_WRAPPER= CARGO_BUILD_JOBS=1 cargo build --release --locked --manifest-path native-rust/Cargo.toml
    python3 scripts/benchmark_ocr.py

open:
    open "$HOME/Applications/Daybook.app"

# Synthetic stall, overload and recovery checks; never starts hardware capture.
test-resilience:
    python3 scripts/vendor_speechworks.py --ensure
    python3 scripts/verify_resilience.py
