// Reads the disc image: a File can only be read synchronously in a worker.
import init, { disc_languages, unpack_disc } from './pkg/grim_web.js';

const ready = init();

onmessage = async ({ data: { file, language } }) => {
  try {
    await ready;
    if (!language) {
      const languages = disc_languages(file);
      if (languages[0] !== 'int') {
        postMessage({ languages });
        return;
      }
      language = 'int';
    }
    unpack_disc(file, language, (path, data, done, total) => {
      postMessage({ path, data: data.buffer, done, total }, [data.buffer]);
    });
    postMessage({ finished: true });
  } catch (e) {
    postMessage({ error: String(e) });
  }
};
