import { copyFileSync, readFileSync, readdirSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const repositoryUrl = 'https://github.com/aceleisureman/mykvm'

export function updaterEndpoint(channel) {
  if (channel === 'beta') {
    return `${repositoryUrl}/releases/download/beta/latest.json`
  }
  if (channel === 'stable') {
    return `${repositoryUrl}/releases/latest/download/latest.json`
  }
  throw new Error(`Unsupported release channel: ${channel}`)
}

function versionCore(version) {
  const match = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.exec(version)
  if (!match) throw new Error(`Invalid release version: ${version}`)
  const core = match.slice(1, 4).map(Number)
  if (!core.every(Number.isSafeInteger)) {
    throw new Error(`Release version is too large: ${version}`)
  }
  return core
}

export function nextReleaseVersion({ sourceVersion, latestStableVersion = '', channel, bump, runNumber }) {
  updaterEndpoint(channel) // Validate the channel before calculating a version.
  let base = versionCore(sourceVersion)
  if (latestStableVersion) {
    const stable = versionCore(latestStableVersion)
    const firstDifference = stable.findIndex((part, index) => part !== base[index])
    if (firstDifference !== -1 && stable[firstDifference] > base[firstDifference]) {
      base = stable
    }
  }

  // A fork may have only beta tags. Never reset it to 0.1.0 below the
  // version of locally installed builds (previously 0.9.8).
  const increment = channel === 'beta' ? 'patch' : bump
  const index = ['major', 'minor', 'patch'].indexOf(increment)
  if (index === -1) throw new Error(`Unsupported version bump: ${bump}`)
  base[index] += 1
  if (!Number.isSafeInteger(base[index])) throw new Error('Release version is too large')
  base.fill(0, index + 1)
  const version = base.join('.')
  if (channel === 'stable') return version
  if (!/^[1-9]\d*$/.test(String(runNumber))) throw new Error('A positive beta run number is required')
  return `${version}-beta.${runNumber}`
}

export function configureRelease({ rootDir, version, channel }) {
  versionCore(version)
  const endpoint = updaterEndpoint(channel)
  for (const file of ['package.json', 'package-lock.json', 'src-tauri/tauri.conf.json']) {
    const path = join(rootDir, file)
    const data = JSON.parse(readFileSync(path, 'utf8'))
    data.version = version
    if (file === 'package-lock.json') data.packages[''].version = version
    if (file === 'src-tauri/tauri.conf.json') {
      // Set BOTH channels explicitly: the dev checkout defaults to beta,
      // but a manually requested stable release must never inherit beta.
      data.plugins.updater.endpoints = [endpoint]
      if (channel === 'beta') data.bundle.macOS.signingIdentity = '-'
    }
    writeFileSync(path, `${JSON.stringify(data, null, 2)}\n`)
  }
  const cargoPath = join(rootDir, 'src-tauri/Cargo.toml')
  const cargo = readFileSync(cargoPath, 'utf8')
  if (!/^version = ".*"$/m.test(cargo)) throw new Error('Cargo package version was not found')
  writeFileSync(cargoPath, cargo.replace(/^version = ".*"$/m, `version = "${version}"`))
}

export function generateUpdaterManifest({ assetDir, repository, version, tag, notes = '', pubDate = new Date().toISOString() }) {
  versionCore(version)
  if (tag !== `v${version}`) throw new Error('Release tag and version do not match')
  if (!/^[\w.-]+\/[\w.-]+$/.test(repository)) throw new Error('Invalid GitHub repository')
  const files = readdirSync(assetDir, { withFileTypes: true })
    .filter((entry) => entry.isFile())
    .map((entry) => entry.name)
    .sort()
  const baseUrl = `https://github.com/${repository}/releases/download/${encodeURIComponent(tag)}`
  const platforms = {}
  const findAsset = (label, predicate) => {
    const matches = files.filter(predicate)
    if (matches.length !== 1) {
      throw new Error(`Expected one ${label} updater artifact, found ${matches.length}`)
    }
    return matches[0]
  }
  const addPlatform = (key, assetName, downloadName = assetName) => {
    const sigName = `${assetName}.sig`
    const signature = files.includes(sigName)
      ? readFileSync(join(assetDir, sigName), 'utf8').trim()
      : ''
    if (!signature) throw new Error(`Missing signature for ${key}: ${assetName}`)
    platforms[key] = { signature, url: `${baseUrl}/${encodeURIComponent(downloadName)}` }
  }

  // The macOS matrix builds a universal app. Do not advertise a single-arch
  // tarball as an update for both architectures.
  const mac = findAsset('universal macOS', (name) => /_universal\.app\.tar\.gz$/i.test(name))
  addPlatform('darwin-aarch64', mac)
  addPlatform('darwin-x86_64', mac)

  const windowsAlias = 'mykvm-windows-x64-setup.exe'
  const windows = findAsset('Windows NSIS', (name) => name !== windowsAlias && /_x64-setup\.exe$/i.test(name))
  addPlatform('windows-x86_64', windows, windowsAlias)

  const appImage = findAsset('Linux x86_64 AppImage', (name) => /(?:amd64|x86_64)\.AppImage$/i.test(name))
  const deb = findAsset('Linux amd64 deb', (name) => /_amd64\.deb$/i.test(name))
  const rpm = findAsset('Linux x86_64 rpm', (name) => /\.x86_64\.rpm$/i.test(name))
  // Retain the generic AppImage key for older clients. Recent Tauri versions
  // prefer os-arch-installer, so a deb install must receive a deb, not AppImage.
  addPlatform('linux-x86_64', appImage)
  addPlatform('linux-x86_64-appimage', appImage)
  addPlatform('linux-x86_64-deb', deb)
  addPlatform('linux-x86_64-rpm', rpm)

  // Validate every required platform before creating any upload aliases.
  copyFileSync(join(assetDir, windows), join(assetDir, windowsAlias))
  copyFileSync(join(assetDir, `${windows}.sig`), join(assetDir, `${windowsAlias}.sig`))
  return { version, notes: notes || `Release ${tag}`, pub_date: pubDate, platforms }
}

function main() {
  const [command, ...args] = process.argv.slice(2)
  switch (command) {
    case 'version': {
      const [channel, bump, runNumber, latestStableVersion = ''] = args
      const sourceVersion = JSON.parse(readFileSync('package.json', 'utf8')).version
      console.log(nextReleaseVersion({ sourceVersion, latestStableVersion, channel, bump, runNumber }))
      break
    }
    case 'configure': {
      const [version, channel] = args
      configureRelease({ rootDir: process.cwd(), version, channel })
      break
    }
    case 'manifest': {
      const [assetDir = 'dl', output = 'latest.json'] = args
      const manifest = generateUpdaterManifest({
        assetDir,
        repository: process.env.REPO,
        version: process.env.VERSION,
        tag: process.env.TAG,
        notes: process.env.RELEASE_NOTES,
      })
      writeFileSync(output, `${JSON.stringify(manifest, null, 2)}\n`)
      console.log(`Wrote ${output} for ${manifest.version}: ${Object.keys(manifest.platforms).join(', ')}`)
      break
    }
    default:
      throw new Error('Usage: release-metadata.mjs version|configure|manifest [arguments]')
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    main()
  } catch (error) {
    console.error(error.message)
    process.exitCode = 1
  }
}
