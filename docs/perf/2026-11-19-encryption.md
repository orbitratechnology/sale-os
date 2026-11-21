# Encryption cost (2026-11-19)

ADR-023 (SQLCipher). The same benchmark (`pos-bench --scale small`, 1,000 samples each) on the
same machine (Intel(R) Xeon(R) Processor @ 2.10GHz, 4 threads, 15 GB RAM, linux), once on plain SQLite (`--plain`) and once encrypted, as the app
runs. Release build. Encrypted connections turn off `cipher_memory_security` and cache 32 MiB
of decrypted pages each. With SQLite's default 2 MiB cache, reads were 4–8 times slower: the
sales list page took 9.5 ms against 1.0 ms.

| Measure | p50 plain | p50 encrypted | p95 plain | p95 encrypted |
|---|---|---|---|---|
| Barcode lookup | 0.1 ms | 0.1 ms | 0.1 ms | 0.2 ms |
| Add item to visible cart | 5.1 ms | 6.5 ms | 8.1 ms | 10.3 ms |
| Product search | 1.9 ms | 1.8 ms | 5.4 ms | 3.5 ms |
| Cash-sale local commit | 4.9 ms | 6.9 ms | 9.1 ms | 10.8 ms |
| 30-day sales report | 2.9 ms | 2.4 ms | 3.8 ms | 3.3 ms |
| Sales list page | 1.1 ms | 1.1 ms | 10.1 ms | 4.2 ms |
| Cold launch (backend) | 6.0 ms | 10.6 ms | 7.9 ms | 13.7 ms |

Reads cost the same once their pages are cached. Writes pay for encrypting and authenticating
each changed page, about 1.5 ms per sale. Opening pays for checking the key, about 4 ms. Every
PRD §14 target keeps a wide margin. The reference-machine run (D-05) is still to do, at full
scale.
