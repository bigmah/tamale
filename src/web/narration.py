# Narration: Gemini speaks each line and Manim mixes it into the video.
import base64, hashlib, io, json, os, re, sys, urllib.request, wave

_API = "https://generativelanguage.googleapis.com/v1beta"
_KEY = os.environ.get("TAMALE_GEMINI_KEY") or os.environ.get("GEMINI_API_KEY")
_VOICE = os.environ.get("TAMALE_GEMINI_VOICE") or "Kore"
_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "voice")
_models = None
NARRATION_FAILED = None


def _call(url, body=None):
    data = json.dumps(body).encode() if body else None
    headers = {"x-goog-api-key": _KEY, "content-type": "application/json"}
    with urllib.request.urlopen(urllib.request.Request(url, data, headers), timeout=120) as r:
        return json.load(r)


def _tts_models():
    global _models
    if _models is None:
        try:
            found = _call(_API + "/models?pageSize=1000").get("models", [])
            names = [m["name"].split("/")[-1] for m in found if "tts" in m["name"]
                     and "generateContent" in m.get("supportedGenerationMethods", [])]
        except Exception:
            names = []
        version = lambda n: float((re.search(r"\d+(\.\d+)?", n) or [0])[0])
        names.sort(key=lambda n: (-version(n), 2 * ("lite" in n) + ("preview" in n), n))
        _models = names[:3] or ["gemini-2.5-flash-preview-tts"]
    return _models


def _speech(text):
    """A WAV of `text` spoken by Gemini (made once per line) and its length in seconds."""
    os.makedirs(_DIR, exist_ok=True)
    name = hashlib.sha1(f"{_VOICE}\n{text}".encode()).hexdigest()[:16]
    path = os.path.join(_DIR, name + ".wav")
    if not os.path.exists(path):
        if not _KEY:
            raise RuntimeError("no Gemini key")
        body = {"contents": [{"parts": [{"text": text}]}], "generationConfig": {
            "responseModalities": ["AUDIO"],
            "speechConfig": {"voiceConfig": {"prebuiltVoiceConfig": {"voiceName": _VOICE}}}}}
        error = None
        for model in _tts_models():
            try:
                parts = _call(f"{_API}/models/{model}:generateContent", body)["candidates"][0]["content"]["parts"]
                audio = next(p["inlineData"] for p in parts if "inlineData" in p)
                break
            except Exception as e:
                error = e
        else:
            raise RuntimeError(error)
        data = base64.b64decode(audio["data"])
        if not data.startswith(b"RIFF"):  # raw 16-bit PCM: give it a WAV header
            rate = re.search(r"rate=(\d+)", audio.get("mimeType", ""))
            buffer = io.BytesIO()
            with wave.open(buffer, "wb") as w:
                w.setnchannels(1)
                w.setsampwidth(2)
                w.setframerate(int(rate[1]) if rate else 24000)
                w.writeframes(data)
            data = buffer.getvalue()
        with open(path, "wb") as f:
            f.write(data)
    with wave.open(path) as w:
        return path, w.getnframes() / w.getframerate()


class narrate:
    """`with narrate(self, "A line."):` around a step's animations: the line plays as the
    step starts, and the step lasts at least as long as the line, plus a short pause."""

    def __init__(self, scene, text, pause=0.4):
        self.scene, self.text, self.pause, self.seconds = scene, text, pause, 0.0

    def __enter__(self):
        global NARRATION_FAILED
        try:
            path, self.seconds = _speech(self.text)
            self.scene.add_sound(path)
        except Exception as e:  # silent, at about the pace it would have been spoken
            self.seconds = max(1.0, len(self.text.split()) / 2.6)
            NARRATION_FAILED = NARRATION_FAILED or str(e)
            print(f"NARRATION FAILED: {e}", file=sys.stderr)
        self.start = self.scene.renderer.time
        return self

    def __exit__(self, *_):
        left = self.start + self.seconds + self.pause - self.scene.renderer.time
        if left > 0.01:
            self.scene.wait(left)
