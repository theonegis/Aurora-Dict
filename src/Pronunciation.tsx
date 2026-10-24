import { Icon } from "./Icon";
import { playPronunciation } from "./audio";
import type { CopyKey } from "./config";

type Translator = (key: CopyKey) => string;

function displayPhonetic(phonetic: string | undefined, stripOuterSlashes: boolean): string | undefined {
  const value = phonetic?.trim();
  if (!value) return undefined;
  return stripOuterSlashes && value.length > 1 && value.startsWith("/") && value.endsWith("/") ? value.slice(1, -1).trim() : value;
}

export function PronunciationRow({ word, fallbackPhonetic, ukPhonetic, usPhonetic, ukAudio, usAudio, stripOuterSlashes = false, t }: {
  word: string; fallbackPhonetic?: string; ukPhonetic?: string; usPhonetic?: string; ukAudio?: string; usAudio?: string; stripOuterSlashes?: boolean; t: Translator;
}) {
  const variants = [
    { label: t("british"), name: t("britishPronunciation"), phonetic: displayPhonetic(ukPhonetic ?? fallbackPhonetic, stripOuterSlashes), audio: ukAudio, lang: "en-GB" },
    { label: t("american"), name: t("americanPronunciation"), phonetic: displayPhonetic(usPhonetic ?? fallbackPhonetic, stripOuterSlashes), audio: usAudio, lang: "en-US" },
  ].filter((variant) => Boolean(variant.phonetic));
  if (!variants.length) return null;
  return <div className="pronunciation-row" aria-label={`${t("british")} / ${t("american")}`}>
    {variants.map((variant) => <span className="pronunciation-variant" key={variant.lang}><b>{variant.label}</b><span className="phonetic">{variant.phonetic}</span><button type="button" className="pronunciation-play" onClick={() => void playPronunciation(word, variant.lang, variant.audio)} aria-label={variant.name} title={variant.name}><Icon name="speaker" size={15} /></button></span>)}
  </div>;
}
