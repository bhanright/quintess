# Opening two long PDFs

How long Quintess and Adobe Acrobat take to open two long documents and settle, and how much memory they use then, measured on the same Windows PC on October 11, 2026. These results cover two files on one computer. They don't measure rendering every page, scrolling through a whole document, or editing, and they will differ with other files, hardware and settings.

Adobe and Acrobat are either registered trademarks or trademarks of Adobe in the United States and/or other countries. Quintess is not affiliated with or endorsed by Adobe.

## Results

Warm medians: the median of runs 2 to 5, after the first run had read the file into the system's file cache.

| Document | Quintess: ready | Acrobat: ready | Quintess memory | Acrobat memory |
|---|---|---|---|---|
| 624-page text document | 0.81 s | 9.20 s | 144 MB | 591 MB |
| 150-page scanned document | 2.66 s | 8.96 s | 387 MB | 807 MB |

Quintess's window appeared in about 0.3 seconds for both documents, with the first page shown. On the scanned document it then spends about two more seconds preparing sharp pages ahead of the one in view (decoding their pictures), within its page memory budget (768 MB by default, set in Preferences), so that scrolling through the document doesn't wait. That work is counted in its ready time.

# The files

Both documents are made by a script in the Quintess repository, so anyone can rebuild them: `python tools/benchmark/make_fixtures.py`.

- **long.pdf**: 624 pages of text (900 sections of six paragraphs), made from Markdown by Quintess. 492,793 bytes, SHA-256 `b92ff6c269e9bd4311d9ad3be8c83a5139bf27b5f7573a24c89febe8b4e11bd9`.
- **scans.pdf**: the first 150 pages of long.pdf rendered at 200 dpi as JPEG pictures and put back into a PDF, as a scanner makes them, with no text layer. 69,949,301 bytes, SHA-256 `95ae9a8b0e1334510423be76b1e543ad718a467aada2634409aa75d41e9b43a7`.

# The computer

- Windows 11 Home, version 10.0.26200.
- AMD Ryzen 7 5700X, 8 cores and 16 threads, running at about 4.4 GHz during the tests.
- 32 GB of memory.
- NVIDIA GeForce RTX 3070 Ti (driver 32.0.16.1062).
- The files were on an Intel SSD (SSDPEKNW020T8, NVMe).
- Quintess 0.2.0 (release build), with default settings: page memory 768 MB, automatic graphics.
- Adobe Acrobat 26.2.21931.0, as installed, with its default settings.

Other programs were open but idle. `tools/benchmark/system_info.ps1` prints these details.

# The method

`tools/benchmark/benchmark.ps1` opens the document in the program five times, closing every process of the program between runs. Each run measures:

- **Window**: the time until the program's window appears.
- **Ready**: the time until the program stops working on its own. Once the window is up, the processes' combined processor time is sampled every quarter second; "ready" is the moment it began growing by less than 0.05 seconds per sample and kept that up for 2 seconds. This doesn't mean every page was rendered.
- **Memory**: the sum of the working sets of every process of the program at that moment, as Task Manager's Details tab shows them. Memory shared between processes is counted once per process. Acrobat ran as four processes (Acrobat and three AcroCEF helpers); Quintess runs as one.

The first run of each program read the file from disk; later runs found it in the system's file cache, so the results above use runs 2 to 5.

# Every run

| Program and document | Run | Window | Ready | Memory | Peak private memory | Processes |
|---|---|---|---|---|---|---|
| Quintess, long.pdf | 1 | 0.90 s | 1.47 s | 162 MB | 214 MB | 1 |
| Quintess, long.pdf | 2 | 0.29 s | 0.81 s | 136 MB | 200 MB | 1 |
| Quintess, long.pdf | 3 | 0.29 s | 0.81 s | 144 MB | 210 MB | 1 |
| Quintess, long.pdf | 4 | 0.29 s | 0.81 s | 144 MB | 210 MB | 1 |
| Quintess, long.pdf | 5 | 0.27 s | 0.80 s | 145 MB | 213 MB | 1 |
| Quintess, scans.pdf | 1 | 0.29 s | 2.66 s | 386 MB | 457 MB | 1 |
| Quintess, scans.pdf | 2 | 0.29 s | 2.66 s | 388 MB | 452 MB | 1 |
| Quintess, scans.pdf | 3 | 0.28 s | 2.65 s | 386 MB | 449 MB | 1 |
| Quintess, scans.pdf | 4 | 0.29 s | 2.66 s | 387 MB | 449 MB | 1 |
| Quintess, scans.pdf | 5 | 0.29 s | 2.66 s | 387 MB | 461 MB | 1 |
| Acrobat, long.pdf | 1 | 0.93 s | 13.63 s | 573 MB | 437 MB | 4 |
| Acrobat, long.pdf | 2 | 0.55 s | 9.25 s | 590 MB | 456 MB | 4 |
| Acrobat, long.pdf | 3 | 0.53 s | 9.20 s | 591 MB | 456 MB | 4 |
| Acrobat, long.pdf | 4 | 0.55 s | 9.20 s | 591 MB | 457 MB | 4 |
| Acrobat, long.pdf | 5 | 0.53 s | 9.21 s | 591 MB | 456 MB | 4 |
| Acrobat, scans.pdf | 1 | 0.55 s | 9.23 s | 808 MB | 675 MB | 4 |
| Acrobat, scans.pdf | 2 | 0.54 s | 8.96 s | 807 MB | 713 MB | 4 |
| Acrobat, scans.pdf | 3 | 0.53 s | 8.97 s | 808 MB | 772 MB | 4 |
| Acrobat, scans.pdf | 4 | 0.54 s | 8.73 s | 803 MB | 670 MB | 4 |
| Acrobat, scans.pdf | 5 | 0.54 s | 9.00 s | 808 MB | 703 MB | 4 |

# Running it yourself

1. Build Quintess, or install it, and run `python tools/benchmark/make_fixtures.py` to make the two documents.
2. Close the program you will measure.
3. Run `powershell -File tools/benchmark/benchmark.ps1 -Program <path to the program> -File <document> -ProcessNames <names>`, with the process names without ".exe" (for Quintess, `quintess`; for Acrobat, `Acrobat,AcroCEF`). The 0.2.0 results above were measured when the app was a separate program, `quintess-app`; from 0.3.0 the app and the command-line tool are one program, and its process is named `quintess`.
4. Run `powershell -File tools/benchmark/system_info.ps1 -Programs <paths>` and keep its output with the results.

The scripts are Windows PowerShell. Corrections and results from other computers are welcome as issues on GitHub.
