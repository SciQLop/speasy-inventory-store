| | Python tree (today) | Python columnar, interned str | Python columnar, one str blob | Rust store |
|---|---|---|---|---|
| build from parsed JSON dict | 0.66 s | 0.73 s | 0.84 s | 0.34 s |
| build from JSON text | 1.02 s | 1.08 s | 1.18 s | 0.77 s |
| build through add() per node | — | 0.73 s | 0.87 s | 0.51 s |
| memory: Python objects + native heap | 192 MB | 37 MB | 42 MB | 48 MB |
| memory: RSS of a process that only loads it | 254 MB | 212 MB | 70 MB | 51 MB |
| JSON export | 0.76 s | 0.53 s | 0.82 s | 0.23 s |
| legacy tree rebuild | 0.00 s | 0.23 s | 0.55 s | 0.19 s |
| legacy pickle (rebuild + protocols 3, 4, 5) | 1.64 s | 0.91 s | 1.20 s | 0.94 s |
| legacy tree, transient memory | 0 MB | 79 MB | 103 MB | 106 MB |
| pickle size (protocol 3) | 63.8 MB | 31.7 MB | 31.8 MB | 32.3 MB |
| uid lookup | 1.03 µs | 0.83 µs | 6.54 µs | 0.83 µs |
| path lookup | 1.38 µs | 12.49 µs | 35.17 µs | 0.84 µs |
| full walk, children + meta dict per node | 0.15 s | 0.12 s | 0.56 s | 0.13 s |
| query: cda parameters in nT | 0.105 s | 0.030 s | 0.122 s | 0.002 s |

query hits: 3435
