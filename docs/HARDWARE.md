# Hardware

## Tiers

| Stage | Machine | Why |
|---|---|---|
| Dev | M4 Mac, UTM ARM VM | Daily development; host runs local model |
| Test | Used ThinkPad (T480/T490/X1 Carbon 6–7/T14 Gen 1–2), **iGPU only**, TPM 2.0 | Real UEFI, TPM, suspend, battery. Update firmware first (T480 Thunderbolt bug). |
| Reference | 16" AMD Strix Halo (Ryzen AI Max+ 395), 128 GB, OLED high-refresh | Open AMD drivers, iGPU (safe to remove Microsoft keys), ~96–120 GB GPU-addressable memory for local models |

## Candidates considered

- **16" Strix Halo OLED** (e.g. SIXUNITED XN77-based models): best fit; verify availability/brand.
- **TUXEDO Stellaris 16 Gen 8** (RTX 5090 24 GB, up to 128 GB RAM, OLED option): fastest for
  mid-size models, Linux-first vendor; Nvidia dGPU complicates owner-key Secure Boot.
- **HP ZBook Ultra G1a** (14", Strix Halo, 128 GB): refined but under 15" and expensive.
- **Framework Laptop 13 Pro**: best "MacBook Pro for Linux" ergonomics; weaker for big models.
- **NovaCustom SecurityTitan V56** (Dasharo coreboot + Heads): maximum firmware ownership,
  but FHD IPS screen and weak AI capacity. Possible future high-assurance profile.
- **Apple Silicon (M1/M2)** via nixos-apple-silicon: secondary dev profile only.
