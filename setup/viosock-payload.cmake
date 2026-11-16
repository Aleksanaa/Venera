# SPDX-License-Identifier: MIT
function(viosock_payload directory output)
  set(files)
  if(directory)
    foreach(name viosock.inf viosock.cat viosock.sys viosocklib_x64.dll viosocklib_x86.dll
                 viosockwspsvc.exe LICENSE.txt viosock-LICENSE.txt viosock-lib-LICENSE.txt)
      if(NOT EXISTS "${directory}/${name}" OR IS_DIRECTORY "${directory}/${name}")
        message(FATAL_ERROR "${directory}/${name} is required for the VirtIO socket package")
      endif()
      list(APPEND files "${directory}/${name}")
    endforeach()
  endif()
  set(${output} "${files}" PARENT_SCOPE)
endfunction()
