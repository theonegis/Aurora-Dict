# Browser regression tests

Run `npm run dev`, open `http://127.0.0.1:1420/tests/review.html`, and click **Run tests**. All five results should say `PASS` and finish with `DONE`.

The harness uses the real React components and browser IndexedDB, mock persistence promises and audio. It never invokes Tauri or changes the desktop SQLite book. Cache tests use `REVIEW-US`/`review-us` in the browser preview's isolated cache.

Coverage: pronunciation without IPA, editor locking and duplicate saves, replacing a quiz generator mid-request, overlapping audio/stale speech fallback, case-sensitive cache keys.

Type-check the harness with `./node_modules/.bin/tsc --project tsconfig.review.json --noEmit`.

Main App integration regressions: open `http://127.0.0.1:1420/tests/app-review.html` and click **Run tests**. All three results should say `PASS`: source switching during cache hydration, independent lookup/translation prompt updates, and adding while the first vocabulary load is pending. Tauri IPC is entirely mocked; local preview settings are restored after the run. No native SQLite files or model engines are touched.


LAN-sync settings visual harness: open `http://127.0.0.1:1420/tests/lan-sync-review.html`. Enable sync, show the placeholder QR, and resolve either conflict version. This uses mocked IPC only; the QR is deliberately not a real invitation. Verified at 650px and 390px without horizontal overflow. No audio or video is loaded.
