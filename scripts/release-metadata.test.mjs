import assert from 'node:assert/strict'
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync, existsSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test from 'node:test'
import { configureRelease, generateUpdaterManifest, nextReleaseVersion, updaterEndpoint } from './release-metadata.mjs'

const versionCases = [
  ['beta-only fork uses source baseline', '0.9.8', '', 'beta', 'patch', '0.9.9-beta.14'],
  ['older stable tag cannot lower source version', '0.9.8', '0.1.0', 'beta', 'patch', '0.9.9-beta.14'],
  ['newer stable tag wins', '0.9.8', '1.0.2', 'beta', 'patch', '1.0.3-beta.14'],
  ['comparison is numeric', '0.9.8', '0.10.0', 'beta', 'patch', '0.10.1-beta.14'],
  ['stable patch without stable tags', '0.9.8', '', 'stable', 'patch', '0.9.9'],
  ['stable minor without stable tags', '0.9.8', '', 'stable', 'minor', '0.10.0'],
  ['stable major from newest baseline', '0.9.8', '1.2.3', 'stable', 'major', '2.0.0'],
  ['prerelease source still gives a newer core', '0.9.9-beta.13', '', 'beta', 'patch', '0.9.10-beta.14'],
]
for (const [name, sourceVersion, latestStableVersion, channel, bump, expected] of versionCases) {
  test(name, () => {
    assert.equal(nextReleaseVersion({ sourceVersion, latestStableVersion, channel, bump, runNumber: '14' }), expected)
  })
}

test('invalid release inputs fail instead of publishing an ambiguous version', () => {
  const valid = { sourceVersion: '0.9.8', channel: 'beta', bump: 'patch', runNumber: '14' }
  for (const override of [
    { sourceVersion: 'unknown' },
    { sourceVersion: '01.2.3' },
    { latestStableVersion: 'not-a-version' },
    { channel: 'unknown' },
    { runNumber: '' },
    { runNumber: '0' },
    { runNumber: '01' },
    { channel: 'stable', bump: 'unknown' },
    { channel: 'stable', bump: 'constructor' },
  ]) {
    assert.throws(() => nextReleaseVersion({ ...valid, ...override }))
  }
})

function tempDirectory(t) {
  const path = mkdtempSync(join(tmpdir(), 'mykvm-release-test-'))
  t.after(() => rmSync(path, { recursive: true, force: true }))
  return path
}

for (const channel of ['stable', 'beta']) {
  test(`${channel} release explicitly selects its own endpoint and preserves the signing key`, (t) => {
    const rootDir = tempDirectory(t)
    mkdirSync(join(rootDir, 'src-tauri'))
    const sourceConfig = JSON.parse(readFileSync(new URL('../src-tauri/tauri.conf.json', import.meta.url), 'utf8'))
    writeFileSync(join(rootDir, 'src-tauri/tauri.conf.json'), JSON.stringify(sourceConfig))
    writeFileSync(join(rootDir, 'package.json'), '{"version":"0.9.8"}')
    writeFileSync(join(rootDir, 'package-lock.json'), '{"version":"0.1.0","packages":{"":{"version":"0.1.0"},"node_modules/example":{"version":"1.0.0"}}}')
    writeFileSync(join(rootDir, 'src-tauri/Cargo.toml'), '[package]\nversion = "0.9.8"\n[dependencies]\nfoo = "1"\n')
    const version = channel === 'beta' ? '0.9.9-beta.14' : '0.9.9'
    configureRelease({ rootDir, version, channel })
    const configured = JSON.parse(readFileSync(join(rootDir, 'src-tauri/tauri.conf.json'), 'utf8'))
    assert.deepEqual(configured.plugins.updater.endpoints, [updaterEndpoint(channel)])
    assert.equal(configured.plugins.updater.pubkey, sourceConfig.plugins.updater.pubkey)
    assert.equal(configured.bundle.createUpdaterArtifacts, true)
    assert.equal(configured.bundle.macOS.signingIdentity, channel === 'beta' ? '-' : sourceConfig.bundle.macOS.signingIdentity)
    assert.equal(configured.version, version)
    assert.equal(JSON.parse(readFileSync(join(rootDir, 'package.json'), 'utf8')).version, version)
    const lock = JSON.parse(readFileSync(join(rootDir, 'package-lock.json'), 'utf8'))
    assert.equal(lock.version, version)
    assert.equal(lock.packages[''].version, version)
    assert.equal(lock.packages['node_modules/example'].version, '1.0.0')
    assert.equal(readFileSync(join(rootDir, 'src-tauri/Cargo.toml'), 'utf8'), `[package]\nversion = "${version}"\n[dependencies]\nfoo = "1"\n`)
  })
}

test('dev checkout uses the working beta channel, without a stable-to-beta fallback', () => {
  const config = JSON.parse(readFileSync(new URL('../src-tauri/tauri.conf.json', import.meta.url), 'utf8'))
  assert.deepEqual(config.plugins.updater.endpoints, [updaterEndpoint('beta')])
  assert.notEqual(updaterEndpoint('stable'), updaterEndpoint('beta'))
})

const version = '0.9.9-beta.14'
const assets = {
  mac: 'mykvm_universal.app.tar.gz',
  windows: `mykvm_${version}_x64-setup.exe`,
  appimage: `mykvm_${version}_amd64.AppImage`,
  deb: `mykvm_${version}_amd64.deb`,
  rpm: `mykvm-${version}-1.x86_64.rpm`,
}
function fixture(t) {
  const assetDir = tempDirectory(t)
  for (const [format, name] of Object.entries(assets)) {
    writeFileSync(join(assetDir, name), `test artifact: ${format}`)
    writeFileSync(join(assetDir, `${name}.sig`), `test-signature-${format}\n`)
  }
  return { assetDir, repository: 'aceleisureman/mykvm', version, tag: `v${version}`, notes: 'Test release', pubDate: '2026-09-08T00:00:00.000Z' }
}

test('each Linux installer receives the matching signed artifact, retaining AppImage compatibility', (t) => {
  const options = fixture(t)
  const manifest = generateUpdaterManifest(options)
  assert.equal(manifest.version, version)
  assert.equal(manifest.notes, options.notes)
  assert.equal(manifest.pub_date, options.pubDate)
  assert.equal(Object.keys(manifest.platforms).length, 7)
  for (const format of ['appimage', 'deb', 'rpm']) {
    const platform = manifest.platforms[`linux-x86_64-${format}`]
    assert.equal(platform.url, `https://github.com/aceleisureman/mykvm/releases/download/v${version}/${assets[format]}`)
    assert.equal(platform.signature, `test-signature-${format}`)
  }
  assert.deepEqual(manifest.platforms['linux-x86_64'], manifest.platforms['linux-x86_64-appimage'])
  assert.notEqual(manifest.platforms['linux-x86_64-deb'].url, manifest.platforms['linux-x86_64'].url)
  assert.deepEqual(manifest.platforms['darwin-aarch64'], manifest.platforms['darwin-x86_64'])
  assert.equal(manifest.platforms['darwin-aarch64'].signature, 'test-signature-mac')
  assert.ok(manifest.platforms['windows-x86_64'].url.endsWith('/mykvm-windows-x64-setup.exe'))
  assert.equal(readFileSync(join(options.assetDir, 'mykvm-windows-x64-setup.exe'), 'utf8'), 'test artifact: windows')
  assert.equal(readFileSync(join(options.assetDir, 'mykvm-windows-x64-setup.exe.sig'), 'utf8').trim(), 'test-signature-windows')
  // Workflow reruns may already have downloaded the Windows aliases.
  assert.deepEqual(generateUpdaterManifest(options), manifest)
})

for (const format of Object.keys(assets)) {
  test(`missing ${format} artifact blocks publishing`, (t) => {
    const options = fixture(t)
    rmSync(join(options.assetDir, assets[format]))
    assert.throws(() => generateUpdaterManifest(options), /Expected one/)
    assert.equal(existsSync(join(options.assetDir, 'mykvm-windows-x64-setup.exe')), false)
  })
  test(`missing or empty ${format} signature blocks publishing`, (t) => {
    const options = fixture(t)
    const signature = join(options.assetDir, `${assets[format]}.sig`)
    rmSync(signature)
    assert.throws(() => generateUpdaterManifest(options), /Missing signature/)
    writeFileSync(signature, ' \n')
    assert.throws(() => generateUpdaterManifest(options), /Missing signature/)
  })
}

test('unrelated architectures are not selected as Linux x86_64 updates', (t) => {
  const options = fixture(t)
  writeFileSync(join(options.assetDir, 'mykvm_0.9.9_arm64.deb'), 'arm deb')
  writeFileSync(join(options.assetDir, 'mykvm_0.9.9_aarch64.AppImage'), 'arm AppImage')
  writeFileSync(join(options.assetDir, 'mykvm-0.9.9-1.aarch64.rpm'), 'arm rpm')
  const manifest = generateUpdaterManifest(options)
  assert.ok(manifest.platforms['linux-x86_64-deb'].url.endsWith(assets.deb))
})

test('ambiguous artifacts and mismatched release tags fail closed', (t) => {
  const options = fixture(t)
  assert.throws(() => generateUpdaterManifest({ ...options, tag: 'v0.1.0-beta.13' }), /tag and version/)
  assert.throws(() => generateUpdaterManifest({ ...options, repository: 'https://unrelated.example/repo' }), /Invalid GitHub repository/)
  writeFileSync(join(options.assetDir, 'mykvm_other_amd64.deb'), 'stale package')
  assert.throws(() => generateUpdaterManifest(options), /Expected one Linux amd64 deb/)
})
