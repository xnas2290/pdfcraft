# Text translation

Select text in the PDF and choose **Translate**. PdfCraft sends only the selected text, source
language and target language to Google Cloud Translation; it does not send the PDF, page image
or surrounding text. The popup has editable source and result boxes, retry/error state,
copy/save controls, and saved translations can be reopened from the popup. Close clears the
temporary translation from the selection menu.

The desktop provider uses the Google Cloud Translation v2 endpoint by default. Create a Google
Cloud Translation API key and configure it in the process environment:

```text
PDFCRAFT_GOOGLE_TRANSLATE_API_KEY=your-key
# Optional, for a compatible proxy or test server:
PDFCRAFT_GOOGLE_TRANSLATE_URL=https://translation.googleapis.com/language/translate/v2
```

The API key is read from the process environment and is never persisted by PdfCraft. Translation
is an external network operation and may be unavailable when offline or when Google rejects the
selected language. Saving creates a blue underlined PDF annotation containing only the translated
text; it is therefore persisted with the document and appears through the normal annotation hover
preview. Favorite languages are stored in PdfCraft Preferences.

The web build currently reports that a translation provider is not configured; its host must
provide an authenticated fetch adapter before enabling outbound translation there.
