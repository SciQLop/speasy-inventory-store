| | Python tree (today) | Python columnar, interned str | Python columnar, one str blob | Rust store |
|---|---|---|---|---|
| build from parsed JSON dict | 0.89 s | 0.68 s | 0.81 s | 0.34 s |
| build from JSON text | 1.38 s | 1.22 s | 1.36 s | 0.85 s |
| build through add() per node | — | 0.71 s | 0.81 s | 0.50 s |
| memory: Python objects + native heap | 207 MB | 65 MB | 70 MB | 78 MB |
| memory: RSS of a process that only loads it | 271 MB | 223 MB | 217 MB | 66 MB |
| JSON export | 0.92 s | 0.62 s | 0.89 s | 0.28 s |
| legacy tree rebuild | 0.00 s | 0.27 s | 0.63 s | 0.26 s |
| legacy pickle (rebuild + protocols 3, 4, 5) | 1.43 s | 1.08 s | 1.47 s | 0.96 s |
| legacy tree, transient memory | 0 MB | 79 MB | 102 MB | 116 MB |
| pickle size (protocol 3) | 63.8 MB | 37.8 MB | 37.9 MB | 33.4 MB |
| uid lookup | 0.95 µs | 0.78 µs | 6.33 µs | 0.76 µs |
| path lookup | 1.36 µs | 11.20 µs | 32.75 µs | 0.74 µs |
| full walk, children + meta dict per node | 0.14 s | 0.12 s | 0.54 s | 0.14 s |
| query: cda parameters in nT | 0.105 s | 0.030 s | 0.115 s | 0.002 s |

query hits: 3435
