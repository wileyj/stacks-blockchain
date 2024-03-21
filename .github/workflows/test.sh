ARCH=(
    linux-glibc  
    linux-musl
    macos 
    windows
)

CPU=(
    armv7
    arm64
    x86-64
    x86-64-v3
)

for arch in ${ARCH[@]}; do
    for cpu in ${CPU[@]}; do
        if  ! [[ 
            ("${arch}" == "windows" && "${cpu}" == "armv7") ||
            ("${arch}" == "windows" && "${cpu}" == "arm64") ||
            ("${arch}" == "macos" && "${cpu}" == "armv7")

        ]]; then
            echo "arch: ${arch}-${cpu}"
            case ${cpu} in
                x86-64)
                    DOCKERFILE_CPU="x64"
                    ARCHIVE_NAME="x64"
                    ;;
                x86-64-v3)
                    DOCKERFILE_CPU="x64"
                    ARCHIVE_NAME="x64-v3"
                    ;;
                *)
                    DOCKERFILE_CPU="${cpu}"
                    ARCHIVE_NAME="${cpu}"
                    ;;
            esac
            echo "  Dockerfile: Dockerfile.${arch}.${DOCKERFILE_CPU}"
            echo "  Archive: ${arch}_${ARCHIVE_NAME}"
        fi
    done
done



        #   CPU="${{ matrix.cpu }}"
        #   if [[ "${{ matrix.cpu }}" == "**x86**" ]]; then
        #     CPU="x64"
        #   fi
        #   echo "dockerfile = Dockerfile.${{ matrix.arch }}-$CPU"
        #   echo "DOCKERFILE=Dockerfile.${{ matrix.arch }}-${CPU}" >> "$GITHUB_ENV"
