# Quelana Assistant

This project is a minimal Rust assistant that connects to a Gemini-compatible API. It is intentionally unobtrusive and only sends queries when you trigger it (press Enter in the prompt), so it does not record or monitor meetings without your explicit action.

Setup
- Create a Google/other Gemini-compatible API endpoint and obtain an API key.
- Create a `.env` file in the project root with the following keys:

```
GEMINI_API_KEY=your_api_key_here
GEMINI_API_URL=https://your-gemini-endpoint.example.com/generate
```

Run

```bash
cargo run --release
```

Usage
- Keep the terminal minimized during meetings. When someone asks a question you want to answer, press Enter in the running app, type/paste the question, and the assistant will query Gemini and print a suggested reply.

Notes
- This is an MVP focused on privacy and explicit user control. Next steps: integrate push-to-talk speech-to-text, add a system tray icon and global hotkey, and implement a small overlay UI for quick acceptance of suggested replies.
