CREATE FUNCTION vpic.fvincheckdigit(strvin character varying) RETURNS character varying
    LANGUAGE plpgsql
    AS $$
declare
	TempString VARCHAR(4) = '';
	sVINChar VARCHAR(1) = '';
	patternDefault VARCHAR(50)		= '[a-h,j-n,p,r-z,0-9]';
	patternMY VARCHAR(50)			= '[a-h,j-n,p,r-t,v-y,1-9]';
	patternNumbersOnly VARCHAR(50)	= '[0-9]';
	pattern VARCHAR(50);
	temp REAL;
    TempDigit REAL = 0;
    CalcDigit REAL = 0;
	CalcTemp INT = 0;
	i INT;
	valid boolean;
begin

	i = 1;
	if length(strVin) = 17 then
		CalcDigit = 0;

		while i <= length(strVIN) loop
			sVINChar = SUBSTRING(strVIN, i, 1);

			CASE
		        WHEN i = 10 THEN
					pattern = patternMY;
				WHEN i in (13, 14) and SUBSTRING(strVIN, 3, 1) = '9' THEN
					pattern = patternDefault;
				WHEN i in (13, 14) and SUBSTRING(strVIN, 3, 1) <> '9' THEN
					pattern = patternNumbersOnly;
				WHEN i >= 15 THEN
					pattern = patternNumbersOnly;
			ELSE
				pattern = patternDefault;
			END CASE;

			if not sVINChar ~* pattern then
				return '?';
			end if;

			CASE sVINChar
				WHEN '0' THEN CalcTemp = 0;
				WHEN '1' THEN CalcTemp = 1;
				WHEN '2' THEN CalcTemp = 2;
				WHEN '3' THEN CalcTemp = 3;
				WHEN '4' THEN CalcTemp = 4;
				WHEN '5' THEN CalcTemp = 5;
				WHEN '6' THEN CalcTemp = 6;
				WHEN '7' THEN CalcTemp = 7;
				WHEN '8' THEN CalcTemp = 8;
				WHEN '9' THEN CalcTemp = 9;
				WHEN 'A' THEN CalcTemp = 1;
				WHEN 'B' THEN CalcTemp = 2;
				WHEN 'C' THEN CalcTemp = 3;
				WHEN 'D' THEN CalcTemp = 4;
				WHEN 'E' THEN CalcTemp = 5;
				WHEN 'F' THEN CalcTemp = 6;
				WHEN 'G' THEN CalcTemp = 7;
				WHEN 'H' THEN CalcTemp = 8;
				WHEN 'J' THEN CalcTemp = 1;
				WHEN 'K' THEN CalcTemp = 2;
				WHEN 'L' THEN CalcTemp = 3;
				WHEN 'M' THEN CalcTemp = 4;
				WHEN 'N' THEN CalcTemp = 5;
				WHEN 'P' THEN CalcTemp = 7;
				WHEN 'R' THEN CalcTemp = 9;
				WHEN 'S' THEN CalcTemp = 2;
				WHEN 'T' THEN CalcTemp = 3;
				WHEN 'U' THEN CalcTemp = 4;
				WHEN 'V' THEN CalcTemp = 5;
				WHEN 'W' THEN CalcTemp = 6;
				WHEN 'X' THEN CalcTemp = 7;
				WHEN 'Y' THEN CalcTemp = 8;
				WHEN 'Z' THEN CalcTemp = 9;
			ELSE
				CalcTemp = -1;
			END CASE;

			CASE i
				WHEN 1 THEN CalcDigit = CalcDigit + (CalcTemp * 8);
				WHEN 2 THEN CalcDigit = CalcDigit + (CalcTemp * 7);
				WHEN 3 THEN CalcDigit = CalcDigit + (CalcTemp * 6);
				WHEN 4 THEN CalcDigit = CalcDigit + (CalcTemp * 5);
				WHEN 5 THEN CalcDigit = CalcDigit + (CalcTemp * 4);
				WHEN 6 THEN CalcDigit = CalcDigit + (CalcTemp * 3);
				WHEN 7 THEN CalcDigit = CalcDigit + (CalcTemp * 2);
				WHEN 8 THEN CalcDigit = CalcDigit + (CalcTemp * 10);
				WHEN 9 THEN CalcDigit = CalcDigit;
				WHEN 10 THEN CalcDigit = CalcDigit + (CalcTemp * 9);
				WHEN 11 THEN CalcDigit = CalcDigit + (CalcTemp * 8);
				WHEN 12 THEN CalcDigit = CalcDigit + (CalcTemp * 7);
				WHEN 13 THEN CalcDigit = CalcDigit + (CalcTemp * 6);
				WHEN 14 THEN CalcDigit = CalcDigit + (CalcTemp * 5);
				WHEN 15 THEN CalcDigit = CalcDigit + (CalcTemp * 4);
				WHEN 16 THEN CalcDigit = CalcDigit + (CalcTemp * 3);
				WHEN 17 THEN CalcDigit = CalcDigit + (CalcTemp * 2);
			ELSE
				CalcDigit = CalcDigit;
			END CASE;

			i = i + 1;
		end loop;

		temp = CalcDigit / 11;
		TempDigit = ROUND(CAST((temp - CAST(floor(temp) AS int)) * 11 as NUMERIC), 2);
		TempString = CAST(TempDigit as VARCHAR(10));
		TempString = CASE LENGTH(TRIM(TempString))
		                	WHEN 1 THEN ' ' || TempString
		                 ELSE TempString
		                 END;
		if TempString = '10' then
			TempString = 'X';
		else
			TempString = SUBSTRING(TempString, 2, 1);
		end if;
	end if;

	return TempString;
end;
$$;
