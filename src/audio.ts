let activeAudio: HTMLAudioElement | null = null;
let playbackGeneration = 0;

export async function playPronunciation(word: string, language: string, audioUrl = ""): Promise<void> {
  const spokenWord = word.trim();
  if (!spokenWord) return;
  const generation = ++playbackGeneration;
  activeAudio?.pause();
  activeAudio = null;
  if ("speechSynthesis" in window) window.speechSynthesis.cancel();
  if (audioUrl) {
    const audio = new Audio(audioUrl.replace(/^http:\/\/dict\.youdao\.com\//, "https://dict.youdao.com/"));
    activeAudio = audio;
    audio.preload = "auto";
    try { await audio.play(); return; } catch { /* Fall back only for the latest request. */ }
  }
  if (generation !== playbackGeneration || !("speechSynthesis" in window)) return;
  const utterance = new SpeechSynthesisUtterance(spokenWord);
  utterance.lang = language;
  utterance.voice = window.speechSynthesis.getVoices().find((voice) => voice.lang.toLowerCase().startsWith(language.toLowerCase())) ?? null;
  window.speechSynthesis.speak(utterance);
}
