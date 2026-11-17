# Character sets for the CJK and Hangul font subsets (see README.md): the characters of
# GB 2312, JIS X 0208 and KS X 1001, read from Python's codecs.
import sys
def charset(codec, lead, trail):
    out=set()
    for a in lead:
        for b in trail:
            try:
                s=bytes([a,b]).decode(codec)
            except UnicodeDecodeError:
                continue
            out.update(s)
    return out
r=range(0xA1,0xFF)
gb=charset('gb2312', r, r)
jis=charset('euc_jp', r, r)
ks=charset('euc_kr', r, r)
hangul={c for c in ks if 0xAC00<=ord(c)<=0xD7A3 or 0x3130<=ord(c)<=0x318F}
extra=set(chr(c) for c in list(range(0x3000,0x3040))+list(range(0x3040,0x3100))+list(range(0xFF00,0xFFF0))+list(range(0x31F0,0x3200)))
han=( gb|jis|extra ) - hangul
print(len(gb),len(jis),len(ks),len(hangul),len(han), file=sys.stderr)
open('han.txt','w').write(''.join(sorted(han)))
open('hangul.txt','w').write(''.join(sorted(hangul|{chr(c) for c in range(0x3131,0x318F)})))
