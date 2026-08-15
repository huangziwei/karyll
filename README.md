# Karyll

Karyll is a minimalistic writing app with bluetooth keyboard for jailbroken Kindles.

Tested on Kindle Scribe (2022; firmware 5.19.4.0.1), Kindle Colorsoft (firmware
5.18.0.2) and Kindle Oasis 2 (firmware 5.16.2.1.1).

## Build

```sh
git clone https://github.com/huangziwei/karyll && cd karyll/
./build.sh
```

## Install

Copy both of these into `/mnt/us/` over MTP:

    extensions/karyll    # the app
    documents/Karyll.sh  # the scriptlet home-screen tile


## Screenshots

### Kindle Scribe

<table>
  <tr>
    <td width="50%" align="center">
      <img src=".github/assets/ks1-editor.png" width="100%" alt="The editor, with the action strip along the bottom" /><br />
      <sub>Editor</sub>
    </td>
    <td width="50%" align="center">
      <img src=".github/assets/ks1-files.png" width="100%" alt="The Files panel listing documents" /><br />
      <sub>Files</sub>
    </td>
  </tr>
  <tr>
    <td width="50%" align="center">
      <img src=".github/assets/ks1-config.png" width="100%" alt="The Config panel: keyboard, input, type and screen settings" /><br />
      <sub>Config</sub>
    </td>
    <td width="50%" align="center">
      <img src=".github/assets/ks1-help.png" width="100%" alt="The Help panel listing keyboard shortcuts" /><br />
      <sub>Help</sub>
    </td>
  </tr>
</table>

### Kindle Colorsoft



<!-- Colorsoft and Oasis 2 shots go in their own table, same shape as the one
     above. Two per row keeps each device to the height of two screenshots
     however many are added, and portrait panels sit in the grid without
     stretching the landscape ones.

### Kindle Colorsoft

<table>
  <tr>
    <td width="50%" align="center">
      <img src=".github/assets/colorsoft-editor.png" width="100%" alt="" /><br />
      <sub>Editor</sub>
    </td>
    <td width="50%" align="center">
      <img src=".github/assets/colorsoft-config.png" width="100%" alt="" /><br />
      <sub>Colour</sub>
    </td>
  </tr>
</table>
-->


## Thanks

This project will not be possible without [kindle-hid-passthrough](https://github.com/zampierilucas/kindle-hid-passthrough).