//! Project LZ77 v1: bounded history, hash-chain matching, literal runs and references.
use std::{
    collections::VecDeque,
    io::{self, BufReader, BufWriter, Read, Write},
};

const MAGIC: &[u8; 8] = b"BAKLZ77\x01";
const MIN_MATCH: usize = 8;
const MAX_MATCH: usize = 258;
const MAX_LITERAL: usize = u16::MAX as usize;
const EMPTY: u64 = u64::MAX;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Lz77Options {
    pub(crate) window_kib: u16,
    pub(crate) search_depth: u16,
}
impl Default for Lz77Options {
    fn default() -> Self {
        Self {
            window_kib: 64,
            search_depth: 32,
        }
    }
}
impl Lz77Options {
    pub(crate) fn validate(&self) -> io::Result<()> {
        if !(4..=1024).contains(&self.window_kib) || !(1..=256).contains(&self.search_depth) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "LZ77 window_kib must be 4..=1024 and search_depth must be 1..=256",
            ));
        }
        Ok(())
    }
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn byte<R: Read>(reader: &mut R) -> io::Result<Option<u8>> {
    let mut value = [0];
    loop {
        match reader.read(&mut value) {
            Ok(0) => return Ok(None),
            Ok(_) => return Ok(Some(value[0])),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

pub(super) struct Encoder<W: Write> {
    inner: BufWriter<W>,
    history: Vec<u8>,
    heads: Vec<u64>,
    previous: Vec<u64>,
    position: u64,
    lookahead: VecDeque<u8>,
    literals: Vec<u8>,
    search_depth: usize,
    failed: bool,
}
impl<W: Write> Encoder<W> {
    pub(super) fn new(inner: W, options: &Lz77Options) -> io::Result<Self> {
        options.validate()?;
        let mut inner = BufWriter::with_capacity(128 * 1024, inner);
        inner.write_all(MAGIC)?;
        inner.write_all(&options.window_kib.to_le_bytes())?;
        let window = usize::from(options.window_kib) * 1024;
        Ok(Self {
            inner,
            history: vec![0; window],
            heads: vec![EMPTY; 65536],
            previous: vec![EMPTY; window],
            position: 0,
            lookahead: VecDeque::with_capacity(MAX_MATCH),
            literals: Vec::with_capacity(MAX_LITERAL),
            search_depth: usize::from(options.search_depth),
            failed: false,
        })
    }
    fn hash(&self) -> usize {
        let key = u32::from_le_bytes([
            self.lookahead[0],
            self.lookahead[1],
            self.lookahead[2],
            self.lookahead[3],
        ]);
        (key.wrapping_mul(2_654_435_761) >> 16) as usize
    }
    fn best_match(&self) -> (usize, usize) {
        if self.lookahead.len() < MIN_MATCH {
            return (0, 0);
        }
        let mut candidate = self.heads[self.hash()];
        let mut best = (0, 0);
        for _ in 0..self.search_depth {
            if candidate == EMPTY || candidate >= self.position {
                break;
            }
            let distance = self.position - candidate;
            if distance > self.history.len() as u64 {
                break;
            }
            let distance = distance as usize;
            let mut length = 0;
            while length < self.lookahead.len() {
                let source = if length < distance {
                    self.history[((candidate % self.history.len() as u64) as usize + length)
                        % self.history.len()]
                } else {
                    self.lookahead[length - distance]
                };
                if source != self.lookahead[length] {
                    break;
                }
                length += 1;
            }
            if length >= MIN_MATCH && length > best.1 {
                best = (distance, length);
            }
            if length == self.lookahead.len() {
                break;
            }
            let next = self.previous[(candidate % self.history.len() as u64) as usize];
            if next >= candidate {
                break;
            }
            candidate = next;
        }
        best
    }
    fn flush_literals(&mut self) -> io::Result<()> {
        if !self.literals.is_empty() {
            self.inner.write_all(&[0])?;
            self.inner
                .write_all(&(self.literals.len() as u16).to_le_bytes())?;
            self.inner.write_all(&self.literals)?;
            self.literals.clear();
        }
        Ok(())
    }
    fn emit(&mut self) -> io::Result<()> {
        let (distance, length) = self.best_match();
        let consumed = if length >= MIN_MATCH {
            self.flush_literals()?;
            self.inner.write_all(&[1])?;
            self.inner.write_all(&(distance as u32).to_le_bytes())?;
            self.inner.write_all(&(length as u16).to_le_bytes())?;
            length
        } else {
            self.literals.push(self.lookahead[0]);
            if self.literals.len() == MAX_LITERAL {
                self.flush_literals()?;
            }
            1
        };
        for _ in 0..consumed {
            let next = self
                .position
                .checked_add(1)
                .ok_or_else(|| invalid("LZ77 input position overflow"))?;
            let slot = (self.position % self.history.len() as u64) as usize;
            if self.lookahead.len() >= 4 {
                let hash = self.hash();
                self.previous[slot] = self.heads[hash];
                self.heads[hash] = self.position;
            }
            self.history[slot] = self.lookahead[0];
            let _ = self.lookahead.pop_front();
            self.position = next;
        }
        Ok(())
    }
    fn drain(&mut self) -> io::Result<()> {
        while !self.lookahead.is_empty() {
            self.emit()?;
        }
        self.flush_literals()
    }
    pub(super) fn finish(mut self) -> io::Result<W> {
        if self.failed {
            return Err(io::Error::other("LZ77 encoder previously failed"));
        }
        self.drain()?;
        self.inner.write_all(&[255])?;
        self.inner.flush()?;
        self.inner.into_inner().map_err(|error| error.into_error())
    }
}
impl<W: Write> Write for Encoder<W> {
    fn write(&mut self, mut input: &[u8]) -> io::Result<usize> {
        if self.failed {
            return Err(io::Error::other("LZ77 encoder previously failed"));
        }
        let count = input.len();
        let result = (|| {
            while !input.is_empty() {
                let n = input.len().min(MAX_MATCH - self.lookahead.len());
                self.lookahead.extend(&input[..n]);
                input = &input[n..];
                if self.lookahead.len() == MAX_MATCH {
                    self.emit()?;
                }
            }
            Ok(count)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.failed {
            return Err(io::Error::other("LZ77 encoder previously failed"));
        }
        let result = self.drain().and_then(|()| self.inner.flush());
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}

enum Token {
    Next,
    Literal(usize),
    Reference { distance: usize, remaining: usize },
    End,
}
pub(super) struct Decoder<R> {
    inner: BufReader<R>,
    history: Vec<u8>,
    cursor: usize,
    available: usize,
    token: Token,
    failed: bool,
}
impl<R: Read> Decoder<R> {
    pub(super) fn new(inner: R) -> io::Result<Self> {
        let mut inner = BufReader::with_capacity(128 * 1024, inner);
        let mut magic = [0; 8];
        inner.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(invalid("invalid LZ77 header"));
        }
        let mut window = [0; 2];
        inner.read_exact(&mut window)?;
        let window = u16::from_le_bytes(window);
        if !(4..=1024).contains(&window) {
            return Err(invalid("invalid LZ77 window"));
        }
        Ok(Self {
            inner,
            history: vec![0; usize::from(window) * 1024],
            cursor: 0,
            available: 0,
            token: Token::Next,
            failed: false,
        })
    }
    pub(super) fn finish(self) -> io::Result<R> {
        if self.failed || !matches!(self.token, Token::End) {
            return Err(invalid("LZ77 stream not fully consumed"));
        }
        Ok(self.inner.into_inner())
    }
    fn next(&mut self) -> io::Result<()> {
        self.token =
            match byte(&mut self.inner)?.ok_or_else(|| invalid("missing LZ77 end marker"))? {
                0 => {
                    let mut length = [0; 2];
                    self.inner.read_exact(&mut length)?;
                    let length = usize::from(u16::from_le_bytes(length));
                    if length == 0 {
                        return Err(invalid("empty LZ77 literal run"));
                    }
                    Token::Literal(length)
                }
                1 => {
                    let mut distance = [0; 4];
                    let mut length = [0; 2];
                    self.inner.read_exact(&mut distance)?;
                    self.inner.read_exact(&mut length)?;
                    let distance = u32::from_le_bytes(distance) as usize;
                    let length = usize::from(u16::from_le_bytes(length));
                    if distance == 0
                        || distance > self.available
                        || !(MIN_MATCH..=MAX_MATCH).contains(&length)
                    {
                        return Err(invalid("invalid LZ77 reference"));
                    }
                    Token::Reference {
                        distance,
                        remaining: length,
                    }
                }
                255 => {
                    if byte(&mut self.inner)?.is_some() {
                        return Err(invalid("trailing LZ77 data"));
                    }
                    Token::End
                }
                _ => return Err(invalid("unknown LZ77 token")),
            };
        Ok(())
    }
    fn read_inner(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let mut count = 0;
        while count < output.len() {
            let value = match &mut self.token {
                Token::Next => {
                    self.next()?;
                    continue;
                }
                Token::End => break,
                Token::Literal(remaining) => {
                    let value =
                        byte(&mut self.inner)?.ok_or_else(|| invalid("truncated LZ77 literal"))?;
                    *remaining -= 1;
                    if *remaining == 0 {
                        self.token = Token::Next;
                    }
                    value
                }
                Token::Reference {
                    distance,
                    remaining,
                } => {
                    let value = self.history
                        [(self.cursor + self.history.len() - *distance) % self.history.len()];
                    *remaining -= 1;
                    if *remaining == 0 {
                        self.token = Token::Next;
                    }
                    value
                }
            };
            output[count] = value;
            count += 1;
            self.history[self.cursor] = value;
            self.cursor = (self.cursor + 1) % self.history.len();
            self.available = (self.available + 1).min(self.history.len());
        }
        Ok(count)
    }
}
impl<R: Read> Read for Decoder<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if self.failed {
            return Err(invalid("LZ77 decoder previously failed"));
        }
        let result = self.read_inner(output);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::io::Cursor;
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn encode(
        data: &[u8],
        options: &Lz77Options,
        chunk: usize,
        flush: bool,
    ) -> io::Result<Vec<u8>> {
        let mut encoder = Encoder::new(Vec::new(), options)?;
        for part in data.chunks(chunk) {
            encoder.write_all(part)?;
            if flush {
                encoder.flush()?;
            }
        }
        encoder.finish()
    }
    fn decode(data: &[u8]) -> io::Result<Vec<u8>> {
        let mut decoder = Decoder::new(Cursor::new(data))?;
        let mut output = Vec::new();
        decoder.read_to_end(&mut output)?;
        decoder.finish()?;
        Ok(output)
    }
    // Independent whole-output reference, without ring buffers, hash chains or production parsing.
    fn reference_decode(encoded: &[u8]) -> io::Result<(Vec<u8>, Vec<usize>)> {
        if encoded.len() < 10 || &encoded[..8] != b"BAKLZ77\x01" {
            return Err(invalid("header"));
        }
        let window = usize::from(u16::from_le_bytes([encoded[8], encoded[9]])) * 1024;
        let mut input = &encoded[10..];
        let mut output = Vec::new();
        let mut distances = Vec::new();
        loop {
            let mut tag = [0];
            input.read_exact(&mut tag)?;
            match tag[0] {
                0 => {
                    let mut length = [0; 2];
                    input.read_exact(&mut length)?;
                    let length = usize::from(u16::from_le_bytes(length));
                    if length == 0 {
                        return Err(invalid("literal"));
                    }
                    let mut literal = vec![0; length];
                    input.read_exact(&mut literal)?;
                    output.extend(literal);
                }
                1 => {
                    let mut distance = [0; 4];
                    let mut length = [0; 2];
                    input.read_exact(&mut distance)?;
                    input.read_exact(&mut length)?;
                    let distance = u32::from_le_bytes(distance) as usize;
                    let length = usize::from(u16::from_le_bytes(length));
                    if distance == 0
                        || distance > window
                        || distance > output.len()
                        || !(8..=258).contains(&length)
                    {
                        return Err(invalid("reference"));
                    }
                    distances.push(distance);
                    for _ in 0..length {
                        output.push(output[output.len() - distance]);
                    }
                }
                255 if input.is_empty() => return Ok((output, distances)),
                _ => return Err(invalid("tag or trailing")),
            }
        }
    }
    fn header(window: u16) -> Vec<u8> {
        [MAGIC.as_slice(), window.to_le_bytes().as_slice()].concat()
    }

    #[test]
    fn golden_stream_and_overlapping_references() -> TestResult {
        let stream = [
            header(4),
            vec![0, 4, 0],
            b"ABCD".to_vec(),
            vec![1, 4, 0, 0, 0, 8, 0, 255],
        ]
        .concat();
        assert_eq!(decode(&stream)?, b"ABCDABCDABCD");
        let repeated = vec![b'a'; 100_000];
        let encoded = encode(&repeated, &Lz77Options::default(), 7919, false)?;
        let (restored, distances) = reference_decode(&encoded)?;
        assert_eq!(restored, repeated);
        assert!(distances.contains(&1));
        assert!(encoded.len() < repeated.len() / 10);
        Ok(())
    }

    #[test]
    fn windows_lengths_input_boundaries_and_flushes() -> TestResult {
        for size in [0, 1, 7, 8, 257, 258, 259, 4096, 65535, 65536, 140_000] {
            let input: Vec<u8> = (0..size)
                .map(|i| ((i * 17 + i / 257) % 251) as u8)
                .collect();
            let options = Lz77Options {
                window_kib: 5,
                search_depth: 16,
            };
            let encoded = encode(&input, &options, 4093, false)?;
            assert_eq!(reference_decode(&encoded)?.0, input);
            assert_eq!(decode(&encoded)?, input);
            assert_eq!(encode(&input, &options, 701, false)?, encoded);
            if size < 10000 {
                assert_eq!(decode(&encode(&input, &options, 13, true)?)?, input);
            }
        }
        // Unique leading keys force the last match to refer back exactly one window.
        let mut state = 17u32;
        let mut input = vec![0; 4096];
        for value in &mut input {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            *value = (state >> 24) as u8;
        }
        let prefix = input[..258].to_vec();
        input.extend(&prefix);
        let encoded = encode(
            &input,
            &Lz77Options {
                window_kib: 4,
                search_depth: 256,
            },
            31,
            false,
        )?;
        let (restored, distances) = reference_decode(&encoded)?;
        assert_eq!(restored, input);
        assert!(distances.contains(&4096));
        Ok(())
    }

    #[test]
    fn maximum_window_and_wide_distances() -> TestResult {
        let mut state = 29u32;
        let mut input = vec![0; 1024 * 1024];
        for value in &mut input {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            *value = (state >> 24) as u8;
        }
        let prefix = input[..258].to_vec();
        input.extend(prefix);
        let encoded = encode(
            &input,
            &Lz77Options {
                window_kib: 1024,
                search_depth: 256,
            },
            128 * 1024,
            false,
        )?;
        let (restored, distances) = reference_decode(&encoded)?;
        assert_eq!(restored, input);
        assert!(distances.contains(&(1024 * 1024)));
        assert_eq!(decode(&encoded)?, input);
        Ok(())
    }

    #[test]
    fn invalid_frames_and_parameters_are_rejected() -> TestResult {
        let valid = [header(4), vec![0, 1, 0, b'a', 1, 1, 0, 0, 0, 8, 0, 255]].concat();
        for length in 0..valid.len() {
            assert!(decode(&valid[..length]).is_err(), "truncation {length}");
        }
        for suffix in [vec![0], vec![255], vec![0, 0, 0]] {
            let mut data = valid.clone();
            data.extend(suffix);
            assert!(decode(&data).is_err());
        }
        for token in [
            vec![2],
            vec![0, 0, 0],
            vec![1, 0, 0, 0, 0, 8, 0],
            vec![1, 1, 0, 0, 0, 8, 0],
            vec![0, 1, 0, b'a', 1, 1, 0, 0, 0, 7, 0],
            vec![0, 1, 0, b'a', 1, 1, 0, 0, 0, 3, 1],
        ] {
            assert!(decode(&[header(4), token, vec![255]].concat()).is_err());
        }
        for window in [0, 3, 1025, u16::MAX] {
            assert!(decode(&[header(window), vec![255]].concat()).is_err());
        }
        for options in [
            Lz77Options {
                window_kib: 3,
                search_depth: 32,
            },
            Lz77Options {
                window_kib: 1025,
                search_depth: 32,
            },
            Lz77Options {
                window_kib: 64,
                search_depth: 0,
            },
            Lz77Options {
                window_kib: 64,
                search_depth: 257,
            },
        ] {
            assert!(Encoder::new(Vec::new(), &options).is_err());
        }
        Ok(())
    }

    struct ShortIo<T>(T);
    impl<T: Read> Read for ShortIo<T> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let n = output.len().min(3);
            self.0.read(&mut output[..n])
        }
    }
    impl<T: Write> Write for ShortIo<T> {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            self.0.write(&input[..input.len().min(3)])
        }
        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }
    struct Broken;
    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("read failure"))
        }
    }
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("write failure"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("flush failure"))
        }
    }
    #[test]
    fn short_io_and_failures() -> TestResult {
        let input = vec![42; 10000];
        let mut encoder = Encoder::new(ShortIo(Vec::new()), &Lz77Options::default())?;
        encoder.write_all(&input)?;
        let encoded = encoder.finish()?.0;
        let mut decoder = Decoder::new(ShortIo(Cursor::new(encoded)))?;
        let mut output = Vec::new();
        decoder.read_to_end(&mut output)?;
        assert_eq!(output, input);
        decoder.finish()?;
        assert!(Decoder::new(Broken).is_err());
        assert!(
            Encoder::new(Broken, &Lz77Options::default())?
                .finish()
                .is_err()
        );
        let source = Cursor::new(header(4)).chain(Broken);
        let mut decoder = Decoder::new(source)?;
        assert!(decoder.read(&mut [0; 1]).is_err());
        assert!(decoder.read(&mut [0; 1]).is_err());
        Ok(())
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases:32, rng_seed:proptest::test_runner::RngSeed::Fixed(20261031), ..ProptestConfig::default() })]
        #[test]
        fn arbitrary_bytes_round_trip(input in proptest::collection::vec(any::<u8>(),0..20000), window in 4u16..=64, depth in 1u16..=64, chunk in 1usize..=1024) {
            let options=Lz77Options {window_kib:window,search_depth:depth};
            let encoded=encode(&input,&options,chunk,false)?;
            prop_assert_eq!(&reference_decode(&encoded)?.0,&input);
            prop_assert_eq!(decode(&encoded)?,input);
        }
        #[test]
        fn arbitrary_compressed_bytes_never_panic(data in proptest::collection::vec(any::<u8>(),0..1000)) {
            let mut encoded=header(4);encoded.extend(data);
            // Bound output while exercising malformed reference/literal parsing.
            if let Ok(decoder)=Decoder::new(Cursor::new(encoded)) { let mut out=Vec::new(); let _=decoder.take(100_000).read_to_end(&mut out); }
        }
    }
}

#[cfg(test)]
mod measurements {
    use super::*;
    use std::time::Instant;

    #[test]
    #[ignore = "manual optimized in-memory codec measurement"]
    fn measure_samples() -> Result<(), Box<dyn std::error::Error>> {
        const SIZE: usize = 1024 * 1024;
        let mut text = Vec::new();
        for line in 0..20000 {
            text.extend_from_slice(format!("File {line}: bak stores files, directories, Unix metadata and authenticated backup contents.\n").as_bytes());
        }
        text.truncate(SIZE);
        let mut state = 20261031u32;
        let random: Vec<u8> = (0..SIZE)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state >> 24) as u8
            })
            .collect();
        for (dataset, data) in [
            ("text", text),
            ("repeated", vec![b'a'; SIZE]),
            ("random", random),
        ] {
            for algorithm in ["lz77", "zstd"] {
                let mut encode_seconds = Vec::new();
                let mut decode_seconds = Vec::new();
                let mut encoded_bytes = 0;
                // First cycle is a warm-up; fixture generation and correctness comparison are untimed.
                for cycle in 0..4 {
                    let started = Instant::now();
                    let encoded = if algorithm == "lz77" {
                        let mut encoder = Encoder::new(Vec::new(), &Lz77Options::default())?;
                        for chunk in data.chunks(128 * 1024) {
                            encoder.write_all(chunk)?;
                        }
                        encoder.finish()?
                    } else {
                        let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 3)?;
                        for chunk in data.chunks(128 * 1024) {
                            encoder.write_all(chunk)?;
                        }
                        encoder.finish()?
                    };
                    let encoding = started.elapsed().as_secs_f64();
                    let started = Instant::now();
                    let mut decoded = Vec::new();
                    if algorithm == "lz77" {
                        let mut decoder = Decoder::new(encoded.as_slice())?;
                        decoder.read_to_end(&mut decoded)?;
                        decoder.finish()?;
                    } else {
                        zstd::stream::read::Decoder::new(encoded.as_slice())?
                            .read_to_end(&mut decoded)?;
                    }
                    let decoding = started.elapsed().as_secs_f64();
                    assert_eq!(decoded, data);
                    encoded_bytes = encoded.len();
                    if cycle > 0 {
                        encode_seconds.push(encoding);
                        decode_seconds.push(decoding);
                    }
                }
                println!(
                    "LZ77_SAMPLE {}",
                    serde_json::json!({"dataset":dataset,"algorithm":algorithm,"source_bytes":data.len(),"encoded_bytes":encoded_bytes,"encode_seconds":encode_seconds,"decode_seconds":decode_seconds})
                );
            }
        }
        Ok(())
    }
}
